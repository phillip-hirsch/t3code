# tr3x

tr3x will put T3 Code's orchestration v2 in Rex: every thread in a Rex tab, with model and provider switching mid-thread.

It's an early work in progress. The design is in [PR #1](https://github.com/phillip-hirsch/tr3x/pull/1).

The `tr3x` CLI in `apps/tr3x` runs on [Bun](https://bun.sh) 1.3.14 or later: `bun apps/tr3x/src/cli.ts daemon`, then `bun apps/tr3x/src/cli.ts doctor`. `bun apps/tr3x/src/cli.ts view --new` starts a thread in the current directory, and `view <threadId>` opens one; switch model or provider between runs with ctrl+p. The T3 server it starts runs on Node (`node` on `PATH`, or `TR3X_NODE_BIN`), in `~/.tr3x` (or `TR3X_HOME`), never T3 Code's `~/.t3`.

With the daemon running, each thread can live in a Rex tab, one Rex session per project. `bun apps/tr3x/src/cli.ts setup rex` prints the line to add to Rex's `init.lua` for New Thread (cmd+shift+t), Open Thread… (cmd+shift+o) and Next Blocked Thread (cmd+shift+b). Closing a tab hides its thread; `tr3x open <threadId>` brings it back. Archiving a thread closes its tab.

<sub>Built on [T3 Code](https://github.com/pingdotgg/t3code) by [@t3dotgg](https://github.com/t3dotgg) and [@juliusmarminge](https://github.com/juliusmarminge). Inspired by [t1code](https://github.com/maria-rcks/t1code) by [@maria-rcks](https://github.com/maria-rcks) and [herdr](https://github.com/herdrdev/herdr).</sub>
