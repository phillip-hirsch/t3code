// @effect-diagnostics nodeBuiltinImport:off -- tr3x runs on Bun outside the Effect runtime.
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";

import { findOnPath } from "../executables.ts";

const LUA_MODULE = NodeURL.fileURLToPath(new URL("../../rex/tr3x.lua", import.meta.url));
const CLI = NodeURL.fileURLToPath(new URL("../cli.ts", import.meta.url));

/** Rex's config directory, as Rex finds it on macOS and Linux. */
function rexConfigDir(env: NodeJS.ProcessEnv, userHome: string): string {
  const configured = env.REX_CONFIG?.trim();
  if (configured) return NodePath.dirname(configured);
  return NodePath.join(env.XDG_CONFIG_HOME?.trim() || NodePath.join(userHome, ".config"), "rex");
}

const luaString = (value: string) => JSON.stringify(value);

/** `tr3x setup rex`: the steps to load tr3x.lua. It prints them and never edits Rex's config. */
export function rexSetupInstructions(
  env: NodeJS.ProcessEnv,
  userHome: string = NodeOS.homedir(),
  bun: string = findOnPath("bun", env) ?? process.execPath,
): string {
  const dir = rexConfigDir(env, userHome);
  const requireLine =
    findOnPath("tr3x", env) === undefined
      ? `require("tr3x").setup({ command = { ${luaString(bun)}, ${luaString(CLI)} } })`
      : `require("tr3x")`;
  return `Add tr3x to Rex (tr3x never edits your Rex config):

1. Put the module where Rex's Lua finds it:

     ln -s ${LUA_MODULE} ${NodePath.join(dir, "tr3x.lua")}

2. Add this line to ${NodePath.join(dir, "init.lua")}:

     ${requireLine}

3. Reload Rex's config and start the daemon:

     rex config reload
     tr3x daemon

Then, in the palette or with these keys:
  New Thread           cmd+shift+t   a tab that starts a thread in the focused terminal's directory
  Open Thread…         cmd+shift+o   choose a thread to open
  Next Blocked Thread  cmd+shift+b   go to the next thread waiting on you
`;
}
