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

**Inert subtrees leave the tab order.** The web's settings put locked controls under HTML
`inert`; 0.3.11 has no way to take a subtree out of keyboard focus. The patch:
`InteractiveElement::inert` (`src/elements/div.rs`) brackets the element's paint with
`TabStopMap::begin_inert`/`end_inert` (`src/tab_stop.rs`), which also go in the insertion
history, so a replayed cached view recomputes inertness from its own inert subtrees and the parent
it is replayed in. `TabStopMap::insert` makes a handle inserted inside no tab stop and marks it,
and `Window::draw` drops focus held by a marked handle, so a control locked while focused stops
taking keys. It does this before choosing the native input handler, and installs only a handler
registered for the current focus, because Windows `WM_CHAR` and IME callbacks reach the installed
handler without checking focus. Pointer events are untouched. Tests:
`focus_next_skips_an_inert_subtree`, `cached_view_replay_recomputes_inertness`.

**The interface can scale like a page zoom.** Electron's View menu zoom (and Ctrl+wheel) scales
the whole page; 0.3.11 has only the display's scale factor, and `rem_size` scales nothing drawn
in `px`. The patch adds `Window::set_ui_scale`: the window's logical pixels become the
platform's divided by the scale, so everything draws larger or smaller and stays crisp. Window
bounds and sizes stay in the platform's units. Files:

- `src/window.rs`: the `ui_scale` field, `ui_scale`/`set_ui_scale`; `bounds_changed` multiplies
  the scale factor by it and divides the viewport and pointer by it; the platform input callback
  scales each event (`PlatformInput::scaled_down`) before `dispatch_event`, so events GPUI
  synthesizes in window units aren't scaled twice; `insets` and `visual_viewport_bounds` divide,
  and `show_window_menu`, `set_client_inset`, `set_client_corner_radii`, `set_input_region` and
  the IME position multiply. Accessibility bounds already go out in device pixels.
- `src/interactive.rs`: `PlatformInput::scaled_down`, positions and pixel scroll deltas.
- `src/platform.rs`: `PlatformInputHandler`'s bounds and points the platform asks about.
- `src/geometry.rs`: `Pixels::scale_down`.
- Test in `window.rs`: `ui_scale_scales_the_interface_against_the_platform_window`.

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

## lb-wry 0.53.3

The webview crate (a wry fork) behind the preview browser, file previews and `gpui-webview`.

**A handler for failed navigations.** Electron reports a page that failed to load with
`did-fail-load` and its Chromium net error, which the browser panel shows as its unreachable page
and reports to the server. 0.53.3 reports a failed main-frame navigation only as a finished one,
so the app could tell a failure only by probing for WebView2's `chrome-error:` page, and WebKit's
failures went unseen. The patch adds `WebViewBuilder::with_on_page_load_error_handler`, which
passes a `PageLoadError` (the URL, the platform's error domain and code, its description, and on
Windows the HTTP status when a server answered); the app maps it to a net error
(`preview::net_error`). Files:

- `src/lib.rs`: `PageLoadError`, the attribute and the builder method.
- `src/webview2/mod.rs`: a `NavigationCompleted` handler for unsuccessful navigations, added before
  the page load handler's, so the failure arrives ahead of that navigation's `Finished`.
- `src/wkwebview/class/wry_navigation_delegate.rs`, `src/wkwebview/navigation.rs`,
  `src/wkwebview/mod.rs`: `webView:didFailProvisionalNavigation:withError:` and
  `webView:didFailNavigation:withError:`, with the failing URL from the error's user info.
- `src/webkitgtk/mod.rs`: `load-failed`, with the code read through the error domains a load
  reports (`load_error_code`). It handles the signal, so WebKitGTK loads no error page of its own,
  which would read as a new page load.
- `Cargo.toml`: objc2-foundation's `NSError` and `NSURLError` features.

**A child webview can move to another window.** The browser's separate preview window takes a
tab's live page out of the main window and gives it back when it closes. 0.53.3's `reparent`
takes a different platform type on each OS (an HWND, an `NSWindow` pointer, a GTK container), can
reach none of them from a window handle without `unsafe`, keeps WebView2's old parent for a child
webview, and can't move an X11 child at all. The patch adds `WebView::reparent_as_child`, which
takes a window handle as `build_as_child` does. Files:

- `src/lib.rs`: `reparent_as_child`.
- `src/webview2/mod.rs`: `SetParent` through the existing `reparent`, then the new parent for a
  child webview too, which `focus_parent` and `bounds` use.
- `src/wkwebview/mod.rs`: `addSubview` on the handle's `NSView`, as `new_as_child` does.
- `src/webkitgtk/mod.rs`: `XReparentWindow` of the child's container window, from an Xlib or XCB
  handle.

## gpui-webview 0.9.0

The native WebView the app embeds in GPUI's layout (the preview browser, HTML pages, MCP apps,
media and Mermaid).

**Native views follow the interface scale.** With gpui-pre's `Window::set_ui_scale` (above), a
GPUI pixel is no longer a platform logical pixel. 0.9.0 already places Linux views in device
pixels and matches their page zoom to GPUI's scale factor, but on Windows and macOS it passed
GPUI's pixels to wry as logical ones, so a zoomed window misplaced its pages. The patch, in
`src/lib.rs`, multiplies those bounds by the scale and the page zoom by it too, so a CSS pixel
draws as one GPUI pixel on every platform (its `ui_scale` field, kept current in `prepaint`). The
preview divides its own zoom by the scale, since Electron's app zoom leaves previews alone.

**A webview can move to another window.** For the separate preview window. 0.9.0 has no way to
move its view, and on Linux it forwards page clicks to the window the view was built in, so a
moved page would click the main window. The patch adds `WebView::move_to_window` (through lb-wry's
`reparent_as_child`); on Linux it also points the click forwarding at the new window. Files:

- `src/lib.rs`: `move_to_window` and the `window` cell.
- `src/linux.rs`: `forward_mouse_down` reads the window from that cell.

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

**Copies can carry clipboard metadata.** Copying a composer selection that holds context chips
also copies their records (the web's custom context clipboard format), so a paste into another
draft brings them back. 0.9.0's `Copy` and `Cut` always write plain text. The patch, in
`src/input/base/state.rs`: `InputBaseState::set_copy_metadata` takes a function from the copied
text to the entry's metadata, and `copy` and `cut` write `ClipboardItem::new_string_with_metadata`
when it returns some. One test, `test_copy_and_cut_carry_the_copy_metadata`.

**A textarea can draw a presentation over its text.** The rich composer keeps its text as
Markdown source and draws it the way the web's Tiptap editor renders it: delimiters hidden away
from the caret, list markers and checkboxes in place of their syntax, headings larger, code in the
code font inside a frame. 0.9.0's textarea can only restyle ranges (decorations) and lay out
atomic tokens. The patch adds `TextPresentation`, set with `TextareaState::set_presentation`: none
of it enters the text, its history or the caret's movement, and an edit drops what it touches
and shifts what follows until the caller sets it again.

- Replaced ranges (`InlineReplacement`): hidden (no width), drawn inline at a width (clamped to
  the row), or drawn in the line's indent (a list marker). They wrap and lay out as zero- or
  fixed-width elements beside the tokens, with the same `LineFragment::element` path.
- Line styles (`LineStyle`): a line's font family, a size relative to the input's, and an indent
  on every row; the wrapper measures and narrows the line with them. Rows keep the input's
  line height.
- Backdrops (`BlockBackdrop`): a filled, optionally bordered and rounded rectangle behind the
  rows of the lines a range spans, or a bar beside them, painted before the text.

Files:

- `src/input/base/presentation.rs` (new): the types, `set_presentation` (which drops ranges off
  character boundaries, across lines or over tokens), `adjust_for_edit`; one test,
  `edits_drop_what_they_touch_and_shift_what_follows`.
- `src/input/base/kind.rs`: `TextareaExtras` holds the presentation and a revision;
  `InputExtras::presentation`; `TextareaMode`'s annotation hooks reset and shift it.
- `src/input/base/element.rs`: `measure_tokens` merges replaced widths into the inline metrics
  and passes line styles to the wrapper; a textarea with a presentation lays out through
  `layout_token_lines`, which takes the widths from the merged metrics and applies each line's
  family, size and indent; `prepaint_replacements` draws replacements, `layout_backdrops` the
  backdrops, painted under the glyph backgrounds.
- `src/input/editor/display_map/text_wrapper.rs`: `WrapLineStyle`, `TextWrapper::set_line_styles`
  (rewraps only the lines whose style changed) and the styled wrap; `LineLayout::indent`, added
  to every visual row's offset. `wrap_map.rs`, `display_map.rs` and `display_map/mod.rs` pass it
  through.
- `src/input/mod.rs`: exports the types.

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

**Code block actions span the top row.** The web's code block header puts the language on the
left and its buttons on the right. 0.9.0 pins the actions to the block's top-right corner at their
own width, so the row could span the block only when the caller knew its width. The patch anchors
the actions at the block's left edge too, so the row can fill it. `src/text/node.rs`: the
`actions` container in `CodeBlock::render`.

**`<details>` is a disclosure.** The chat renders `<details>`/`<summary>` as an expandable
disclosure (the web's `MarkdownDetails`): a summary row with a chevron that opens a panel of the
Markdown inside, between hairlines, and the thread find opens a closed one to show a match. 0.9.0
flattens the element's children into ordinary blocks, and CommonMark ends an HTML block at a blank
line, so the open tag, the Markdown inside and `</details>` arrive as sibling blocks. The patch:

- `src/text/format/details.rs` (new): `DetailsGrouper` pairs `<details>` and `</details>` tags
  across a container's sibling blocks, as rehype-raw's re-parse does (nested, several in one HTML
  block, a summary in a later HTML block, one never closed holding the rest of the document, as a
  streamed reply reads); test `tags_are_found_whole_and_case_blind`.
- `src/text/format/markdown.rs`: `block_children` runs the root's, blockquotes' and list items'
  blocks through it; tests `details_tags_hold_the_markdown_blocks_between_them`,
  `details_nest_close_and_open_within_one_html_block`,
  `a_details_element_in_one_html_block_is_a_disclosure`,
  `a_summary_in_its_own_html_block_still_labels_the_disclosure`.
- `src/text/format/html.rs`: a `details` arm and `parse_details`, the first `<summary>` as inline
  text and the rest as blocks.
- `src/text/node.rs`: `BlockNode::Details` (summary, children, the `open` attribute, span) in every
  traversal; `render_details` draws it, the row keyboard-focusable with a focus ring;
  `Disclosures`, the open state the reader toggled, keyed by the disclosure's source start so it
  survives streamed re-parses, reached through `NodeContext::disclosures`.
- `src/text/state.rs`: `TextViewState` owns the `Disclosures`; `reveal_range` opens every
  disclosure whose panel holds the revealed text (the web's `beforematch`); test
  `revealing_text_in_a_closed_disclosure_opens_it`.
- `src/text/range_highlight.rs`, `src/text/stream_fade.rs`: the summary is a text leaf and the
  panel's text stays in the rendered text while closed, so find counts it; the index records each
  panel's range for `reveal_range`.

**Links can drop their underline.** The chat's links show by color alone (`.chat-markdown a {
text-decoration: none }`); 0.9.0 always draws a hairline under every link. The patch adds an
opt-in flag; other views keep the underline. Files:

- `src/text/style.rs`: `TextViewStyle::link_underline`, `with_link_underline`.
- `src/text/text_view.rs`: `TextView::link_underline`, applied to the frame's style.
- `src/text/node.rs`: `link_underline`, read wherever a link mark is styled.

gpui-component's `src/text/compat.rs` forwards `link_underline` from its `TextView`.

**Links report hover.** The chat opens a pull request link's preview card, and shows any web
link's URL in a tooltip, when the pointer rests on it (`PullRequestLinkPreview`). 0.9.0 tracks the
hovered link only to set the cursor. The patch adds `TextView::on_link_hover`: the handler gets
`Some(LinkHover)` (the URL and the line box of the link under the pointer, in window coordinates)
when the pointer enters a link, and `None` when it leaves it. A link wrapped over lines, or led by
an inline object, reports from each piece, so one event can carry a leave and an enter in either
order; the app resolves them together. Files:

- `src/text/text_view.rs`: `LinkHover`, `LinkHoverFn`, the `link_hover_handler` field and builder;
  test `link_hover_reports_entering_and_leaving_a_link`.
- `src/text/state.rs`: the handler, set each frame like the task toggle's.
- `src/text/inline.rs`: the mouse-move listener reports changes, also when the pointer leaves the
  text; `InlineState::reported_hover` keeps the last report across frames, so a repaint under a
  still pointer reports nothing and a link drawn under the pointer reports on the first move;
  `link_line_bounds`. `src/text/node.rs`: `set_leaf_key` initializes the new field.
- `src/text/inline_object.rs`: an object inside a link reports its own bounds.

gpui-component's `src/text/compat.rs` forwards `on_link_hover`, and `src/text/mod.rs` re-exports
`LinkHover`.

**A plugin object can lead a link.** The chat draws a host mark (the GitHub icon, a favicon, a
globe) before a web link's text (`MarkdownExternalLinkContent`), and a click or right click on it
is the link's. 0.9.0's inline plugins replace the node they claim. The patch adds
`MarkdownNode::leading()`: for a claimed link, the converter places the object first with the
link's mark, then converts the link as if no plugin claimed it. The object's text and Markdown stay
empty, so it adds nothing to the text, find or copies. Files:

- `src/text/markdown_ext.rs`: the `leading` flag and builder.
- `src/text/format/markdown.rs`: the leading branch of `parse_paragraph`, `link_text_mark` (shared
  with the `Node::Link` arm); test `a_leading_object_opens_the_link_whose_text_follows_it`.

**Markdown images open on a click.** The web's chat markdown makes an image that is not inside a
link a button (`role="button"`, `tabIndex={0}`) whose click, Enter or Space opens the expanded
image viewer. 0.9.0's images are clickable only inside a link. The patch adds
`TextView::on_image_click`: with a handler set, an image outside a link is a tab stop with a
pointer cursor and a 2px ring on keyboard focus, and a left click, Enter or Space calls the handler
with the image's URL. Linked images keep following their link. Files:

- `src/text/text_view.rs`: `ImageClickHandlerFn`, `on_image_click` and `clickable_image`.
- `src/text/state.rs`: the handler carried to each frame's `NodeContext`.
- `src/text/node.rs`: `NodeContext::image_click_handler`, the plain image path in
  `Paragraph::render`, and the image's URL on `InlineFlowItem::Image`.
- `src/text/inline_flow.rs`: `InlineFlow::image_click_handler` and `image_element`.

**Code blocks can draw as something else.** The web's chat draws a closed Mermaid fence as its
diagram, with the source a toggle away. 0.9.0 always draws a fenced block as code. The patch adds
`TextView::code_block_renderer`: the closure gets each `CodeBlock` and returns the element that
replaces it, keeping the block's spacing, or `None` to draw the block as usual. Files:

- `src/text/text_view.rs`: `CodeBlockRenderFn`, the `code_block_renderer` field and builder.
- `src/text/state.rs`: the renderer, carried to each frame's `NodeContext`.
- `src/text/node.rs`: `NodeContext::code_block_renderer`, asked first in `CodeBlock::render`.

gpui-component's `src/text/compat.rs` forwards `code_block_renderer` from its `TextView`.

**Wide tables scroll, with a scrollbar.** The web's chat tables sit in a scroll area whose
horizontal scrollbar stays visible while they overflow, its thumb `muted-foreground` at 60% (80%
hovered). 0.9.0 scrolls only tables whose style opts into `overflow_x: scroll`, clips the others
past their column floors, and draws no scrollbar on either. The patch: `src/text/node.rs`
(`BlockNode::render_table`) puts every table in `horizontal_scroll_area`, keeping each layout's
column floors, and `src/scrollable_mask.rs` (`horizontal_scroll_area`) adds an always-shown
horizontal `Scrollbar` in the text style's muted color, 1px off the bottom edge.

The crate's own tests need `--features test-support` and `include_str!` a `README.md` four levels
above `src/text`, which this folder lacks: run them from a scratch copy laid out as
`<dir>/crates/gpui-base` with a `<dir>/README.md`, with `[patch.crates-io]` pointing `gpui-pre` and
`gpui-pre-linux` at the vendored copies (`--config 'patch.crates-io.gpui-pre.path="…"'`).

## gpui-component 0.9.0

The crate's tests need `--features test-support` and `include_bytes!` files of the upstream
monorepo: run them from a scratch workspace with this folder as `<dir>/crates/ui`, gpui-kit-assets
as `crates/assets`, the vendored gpui-base as `crates/base`, stub `examples/fixtures/test.md` and
`themes/aurora.json`, and `[patch.crates-io]` entries for the vendored gpui crates.

**The diff view draws like the web's.** The web renders diffs with Pierre diffs, whose unified
view shows one line number per row (the new line, or the old one for a deletion), marks changed
rows with a 4px bar at the leading edge (dashed for deletions) and separates hunks with an
"N unmodified lines" bar. 0.9.0's Unified mode always draws both number lanes, its bars sit after
the numbers, and its line-info separator reads "N lines not in patch" on a flat strip. The patch
draws one lane per Unified row, moves the bar to the row's edge, and draws the separator as the
web's rounded bar with Pierre's wording. The app picks `DiffHunkSeparator::LineInfo` and
`DiffChangeIndicator::Bars`, Pierre's defaults. Files:

- `src/diff/mod.rs`: `code_width`, the lanes and bar in `code_line`, `change_bar`, and the
  `LineInfo` arm of `render_hunk_separator`.
- `locales/ui.yml`: `Diff.HiddenLine` and the English `Diff.HiddenLines`.

**Unchanged lines outside the hunks expand from the full files.** Pierre fills in a partial diff
when the caller gives it `loadDiffFiles` (`canHydrateCollapsedContext`): the line-info separators
get expand chevrons, and a "More unchanged context may be available" separator follows the last
hunk. 0.9.0 keeps only the patch's lines. The patch adds `DiffState::set_contents_loader`: with a
`DiffContentsLoader`, a modified or renamed file's separators expand (up, down or both by the
expansion count, the count expanding both ends), the first expansion reads the file and
`DiffFile::with_contents` rebuilds it with every line of both sides, and each gap between hunks
shows its revealed ends (`GapReveal`) around a separator that hides the rest. A failed read leaves
the patch as it was. `DiffFile::is_same` also matches a hydrated file to the patch-only file it
came from, so `update_files` keeps it. Files:

- `src/diff/document.rs`: `with_contents`, `can_load_contents`, `is_hydrated`, `gap_pairs`,
  `segments`, each hunk's modified source lines, and `is_same`.
- `src/diff/state.rs`: `DisplayRow::Hunk`'s `hidden` pairs and the trailing separator in
  `project_file`, `GapReveal`, `set_contents_loader`, `expand_gap`, `load_contents`, `hydrate`,
  and `FoldExpansion::Both`.
- `src/diff/mod.rs`: the expandable `LineInfo` arm of `render_hunk_separator`.
- `src/diff/parser.rs`, `src/diff/presentation.rs`: hunks record both sides' lines; a hydrated
  file highlights in one pass.
- `locales/ui.yml`: `Diff.MoreContext`.
- `src/diff/review_tests.rs`: `gap_reveals_grow_from_either_end_and_stop_at_the_gap`,
  `full_contents_fill_the_gaps_between_hunks`,
  `expanding_a_gap_reads_the_file_once_and_reveals_its_lines`.

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

**A text view style can set the body color.** A `TextView` given a `TextViewStyle` stops following
its container's text color (gpui-base only inherits it when no style is passed), so chat text drew at
the full theme foreground where the web dims it to 90%. `src/text/style.rs` adds
`TextViewStyle::foreground`, and `src/text/compat.rs` applies it over the themed foreground.

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

**A select says whether its menu is open.** The scheduled task editor's Destination picker
replaces its rows in recency order only while the menu is closed, so rows don't move under the
cursor. 0.9.0 keeps the open state private; `src/select.rs` adds `SelectState::is_open`.

**Replacing a select's items keeps the search being typed.** The Destination picker replaces its
rows when a thread is added, removed or renamed, even while its menu is open. 0.9.0's
`SelectState::set_items` shows every new row while the typed query stays in the search field;
`src/select.rs` runs the query over the new items before showing them. Test:
`replacing_items_keeps_the_search_being_typed`.

**Switches can be mixed.** gpui-base's `Switch` reports only on or off; `src/switch.rs` (in
gpui-base) adds `mixed`, exposed as `aria-checked="mixed"` like the web's mixed `Switch`.

**A dropdown menu can open from a shortcut.** The web opens the composer's Workspace menu with
`composer.workspace` (Mod+Shift+X) and the right panel's "+" menu with `rightPanel.new`. 0.9.0's `DropdownMenuPopover` opens only from its trigger.
`src/menu/dropdown_menu.rs` adds `DropdownMenuPopover::open`, passed to its `Popover` as
`Popover::open` is, so a caller can hold the open state with `on_open_change`.

**Text views pass link styling and hover through.** `src/text/compat.rs`: `TextView::link_underline`
and `TextView::on_link_hover` forward to gpui-base's (see its "Links can drop their underline" and
"Links report hover"); `src/text/mod.rs` re-exports `LinkHover`.

**Soft breaks can render as line breaks.** `src/text/compat.rs`: `TextView::markdown_line_breaks`,
passing through to gpui-base's (see its "Soft breaks can render as line breaks").

**A thread's timeline reopens where it was left.** 0.9.0's `MessageScrollerState` scrolls only to
a row's top. `src/message_scroller.rs`: `scroll_to_offset` scrolls to a row and an offset into
it, the `logical_scroll_top` it reported.

**A newly started turn anchors near the top.** The web reserves end space under a sent message
so it can sit near the top while the reply streams in, which needs the rows' measured positions.
`src/message_scroller.rs`: `bounds_for_item` and `viewport_bounds` pass the list's through.

**A diff says which files are near the viewport, and pins the current file's header.** The web
reads a large diff's patches as their headers come within 600px of the viewport, and its file
headers are sticky. The patch, in `src/diff/state.rs` and `src/diff/mod.rs`:

- `DiffState::files_near_viewport`: the file indices with a row in the viewport or within a margin
  below it, counting rows never laid out at the row height.
- `DiffState::sticky_header` and `render_diff`'s `diff-sticky-header` overlay: once the list
  scrolls past a file's header, that header draws pinned over the top of the body, pushed up by
  the next file's header as it arrives; a canvas records its height for the push. The overlay
  renders inside the state's update, so file headers read whether their file is folded from
  `CodePresentation::collapsed`, not from the state.

**A diff file lists its patch body.** The web's review comments name a selection by its rows in
the patch (`reviewCommentContext.ts`) and quote those rows; 0.9.0 keeps a file's lines private.
The patch, in `src/diff/document.rs`: `DiffFile::patch_body`, each line of the patch in order as a
`DiffPatchLine` (its line on each side and its text), exported from `src/diff/mod.rs`. Test: an
assertion in `alignment_covers_each_source_line_once`.

**Markdown images open on a click.** `src/text/compat.rs`: `TextView::on_image_click`, passing
through to gpui-base's (see its "Markdown images open on a click").

**Code blocks can draw as something else.** `src/text/compat.rs`: `TextView::code_block_renderer`,
passing through to gpui-base's (see its "Code blocks can draw as something else").

**A diff can be searched.** T3 Code patches Pierre diffs with a find panel (`CodeViewSearch`):
Mod+F inside a diff opens it on the selected text, the query (with Match Case, Whole Word and
Regexp) runs over every file's lines, including folded files, collapsed context and rows never
laid out, matches paint orange with the current one in the modified colour, and Enter, Shift+Enter,
Mod+G and Mod+Shift+G step through them, wrapping, unfolding the file and expanding the context
that hides one and centering it below the sticky header and the panel. Escape closes it and
focuses the diff. 0.9.0 has no find. The patch:

- `src/diff/search.rs`: `collect_matches` (unified order: a run's deletions, then its additions),
  `step`, the panel (`render_panel`) and the match colours.
- `src/diff/state.rs`: `DiffState::open_search`, `close_search`, `is_search_open`,
  `step_search`, `reveal_match` (a folded file reports `DiffEvent::FileExpanded`), and a search
  that runs again, keeping its current match, after `set_files`, `update_files` and hydration.
- `src/diff/mod.rs`: the `OpenSearch`, `SearchNext`, `SearchPrevious` and `CloseSearch` actions
  and bindings, match highlights in `line_highlights`, the panel in `render_diff`, and
  `reveal_match_horizontally`.
- `locales/ui.yml`: the English `Diff.Search*` strings.
- `src/diff/search_tests.rs`.

**A parsed file reports its hunks and each side's lines.** The app checks a tool call's recorded
patch the way the web reads Pierre's `hunks`, `additionLines` and `deletionLines`: an added file
must have no original lines, a deleted one no modified lines. 0.9.0 keeps both counts
crate-private. `src/diff/document.rs`: `DiffFile::lines_count` is public and `DiffFile::hunk_count`
is new.

**A diff paints on the code surface.** The web's diffs sit on `--code-background` and
`--code-foreground`, which a theme sets apart from the canvas. 0.9.0 paints the theme's background
and foreground. `src/diff/mod.rs`: `DiffCodeColors`, a global the app sets; the diff body, its
pinned header and its hunk separators read it, falling back to the theme's colors without it.
