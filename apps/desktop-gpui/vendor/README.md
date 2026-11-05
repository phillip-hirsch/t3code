# Vendored crates

`gpui-base/` is gpui-base 0.9.0 from crates.io (part of gpui-kit 0.9), copied byte for byte
from `~/.cargo/registry/src/index.crates.io-*/gpui-base-0.9.0` (without Cargo's `.cargo-ok` marker)
and used through `[patch.crates-io]` in `../Cargo.toml`. Keep it identical to the published crate
except for the patches below. `vendor/rustfmt.toml` and the `fmt.ignorePatterns` entry in the
repo's `vite.config.ts` keep formatters off it.

To upgrade gpui-kit: copy the new gpui-base over this folder, re-apply each patch below (or drop
it once upstream has an equivalent), and update the version here and in the `[patch]` comment.

## Patches

None yet.
