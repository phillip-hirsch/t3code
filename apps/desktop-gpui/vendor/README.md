# Vendored crates

`gpui-base/` is gpui-base 0.9.0 from crates.io (part of gpui-kit 0.9), copied byte for byte
from `~/.cargo/registry/src/index.crates.io-*/gpui-base-0.9.0` (without Cargo's `.cargo-ok` marker)
and used through `[patch.crates-io]` in `../Cargo.toml`. Keep it identical to the published crate
except for the patches below. `vendor/rustfmt.toml` and the `fmt.ignorePatterns` entry in the
repo's `vite.config.ts` keep formatters off it.

To upgrade gpui-kit: copy the new gpui-base over this folder, re-apply each patch below (or drop
it once upstream has an equivalent), and update the version here and in the `[patch]` comment.

## Patches

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
