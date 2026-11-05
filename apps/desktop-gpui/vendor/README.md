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
