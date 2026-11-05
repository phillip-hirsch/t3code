export const USAGE = `Usage: tr3x <command>

Commands:
  daemon                    Start T3's server in TR3X_HOME (default ~/.tr3x), or attach to the daemon that runs it
  doctor                    Check the daemon, server, providers, and Rex
  view <threadId>           Open a thread in this terminal
  view --new [--cwd <dir>]  Start a thread in <dir> (default: the current directory); the first message launches it

Rex tabs (the daemon must be running):
  new [--cwd <dir>]         Open a tab that starts a thread in <dir> (default: the current directory)
  open <threadId>           Open the thread's tab, or focus it
  next-blocked              Open or focus the next thread waiting on you
  pick [--client <id>]      Choose a thread to open, archived ones too; Rex app <id> shows its tab
  setup rex                 Print how to add tr3x to Rex's init.lua
`;

export type Command =
  | { readonly kind: "help" }
  | { readonly kind: "daemon" | "doctor" | "next-blocked" | "setup-rex" }
  /** `clientId`: the Rex app that asked, so the picker can show it the thread's session. */
  | { readonly kind: "pick"; readonly clientId: string | undefined }
  | { readonly kind: "view"; readonly threadId: string }
  /** `tr3x view` with no thread: the daemon's tab, which names a launch file in `TR3X_LAUNCH`. */
  | { readonly kind: "launch" }
  | { readonly kind: "new"; readonly cwd: string }
  | { readonly kind: "tab-new"; readonly cwd: string }
  | { readonly kind: "open"; readonly threadId: string };

/** `[]` or `--cwd <dir>`: the directory, or null when `rest` is anything else. */
function parseCwd(rest: ReadonlyArray<string>, cwd: string): string | null {
  if (rest.length === 0) return cwd;
  return rest.length === 2 && rest[0] === "--cwd" && rest[1] ? rest[1] : null;
}

/** A thread id argument: one word that isn't a flag. */
function parseThreadId(rest: ReadonlyArray<string>): string | null {
  const [threadId] = rest;
  return rest.length === 1 && threadId && !threadId.startsWith("-") ? threadId : null;
}

/** The command `argv` names, or null when it names none. */
export function parseArgs(argv: ReadonlyArray<string>, cwd: string): Command | null {
  const [command, ...rest] = argv;
  if (command === "help" || command === "--help" || command === "-h") return { kind: "help" };
  if (
    (command === "daemon" || command === "doctor" || command === "next-blocked") &&
    rest.length === 0
  ) {
    return { kind: command };
  }
  if (command === "pick") {
    if (rest.length === 0) return { kind: "pick", clientId: undefined };
    return rest.length === 2 && rest[0] === "--client" && rest[1]
      ? { kind: "pick", clientId: rest[1] }
      : null;
  }
  if (command === "setup")
    return rest.length === 1 && rest[0] === "rex" ? { kind: "setup-rex" } : null;
  if (command === "new") {
    const dir = parseCwd(rest, cwd);
    return dir === null ? null : { kind: "tab-new", cwd: dir };
  }
  if (command === "open") {
    const threadId = parseThreadId(rest);
    return threadId === null ? null : { kind: "open", threadId };
  }
  if (command !== "view") return null;
  if (rest.length === 0) return { kind: "launch" };
  if (rest[0] === "--new") {
    const dir = parseCwd(rest.slice(1), cwd);
    return dir === null ? null : { kind: "new", cwd: dir };
  }
  const threadId = parseThreadId(rest);
  return threadId === null ? null : { kind: "view", threadId };
}
