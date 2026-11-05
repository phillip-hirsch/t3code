// @effect-diagnostics nodeBuiltinImport:off globalTimers:off -- tr3x runs on Bun outside the Effect runtime.
import * as NodeChildProcess from "node:child_process";
import * as NodeReadline from "node:readline";
import * as Schema from "effect/Schema";

import { findOnPath, isExecutableFile } from "../executables.ts";

export const REX_APP_BUNDLE_CLIS = [
  "/Applications/Rex Beta.app/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex",
  "/Applications/Rex.app/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex",
];

/** `TR3X_REX_BIN`, then `rex` on PATH, then the CLI inside the macOS app bundle. */
export function resolveRexCli(
  env: NodeJS.ProcessEnv,
  appBundleClis: ReadonlyArray<string> = REX_APP_BUNDLE_CLIS,
): string | undefined {
  const configured = env.TR3X_REX_BIN?.trim();
  if (configured && isExecutableFile(configured)) return configured;
  return findOnPath("rex", env) ?? appBundleClis.find(isExecutableFile);
}

/** The session and window that show a block, as Rex reports them after creating it. */
export interface RexPlacement {
  readonly sessionId: string;
  readonly windowId: string;
  readonly blockId: string;
}

/** A block in a session's view, with the window that holds it. */
export interface RexBlock extends RexPlacement {
  readonly label: string;
}

/** The session and block events tr3x acts on; Rex sends many more. */
export type RexEvent =
  | { readonly kind: "block_closed"; readonly sessionId: string; readonly blockId: string }
  | { readonly kind: "session_destroyed"; readonly sessionId: string };

const SessionList = Schema.Struct({
  sessions: Schema.Array(Schema.Struct({ session_id: Schema.String, label: Schema.String })),
});
// An empty session's view has no `windows` at all.
const SessionView = Schema.Struct({
  windows: Schema.optional(
    Schema.Array(
      Schema.Struct({
        window_id: Schema.String,
        layers: Schema.Array(
          Schema.Struct({
            blocks: Schema.Array(Schema.Struct({ block_id: Schema.String, label: Schema.String })),
          }),
        ),
      }),
    ),
  ),
});
const CreatedWindow = Schema.Struct({
  window_id: Schema.String,
  block_ids: Schema.Array(Schema.String),
});
const CreatedSession = Schema.Struct({
  session_id: Schema.String,
  initial_windows: Schema.Array(CreatedWindow),
});
const RawEvent = Schema.Struct({
  type: Schema.String,
  payload: Schema.optional(
    Schema.Struct({
      session_id: Schema.optional(Schema.String),
      event: Schema.optional(
        Schema.Struct({
          type: Schema.String,
          payload: Schema.optional(Schema.Struct({ block_id: Schema.optional(Schema.String) })),
        }),
      ),
    }),
  ),
});
const decodeSessionList = Schema.decodeUnknownSync(SessionList);
const decodeSessionView = Schema.decodeUnknownSync(SessionView);
const decodeCreatedWindow = Schema.decodeUnknownSync(CreatedWindow);
const decodeCreatedSession = Schema.decodeUnknownSync(CreatedSession);
const decodeRawEvent = Schema.decodeUnknownOption(Schema.fromJsonString(RawEvent));

/** Reads the one Rex event tr3x cares about from a `rex events --json` line, if it is one. */
export function parseRexEvent(line: string): RexEvent | undefined {
  const raw = decodeRawEvent(line);
  if (raw._tag === "None") return undefined;
  const { type, payload } = raw.value;
  const sessionId = payload?.session_id;
  if (sessionId === undefined) return undefined;
  if (type === "session_destroyed") return { kind: "session_destroyed", sessionId };
  const blockId = payload?.event?.payload?.block_id;
  if (type === "session_event" && payload?.event?.type === "block_closed" && blockId) {
    return { kind: "block_closed", sessionId, blockId };
  }
  return undefined;
}

/** What a new terminal block runs: argv with no shell, in `cwd`. */
export interface RexCommand {
  readonly argv: ReadonlyArray<string>;
  readonly cwd: string;
}

function terminalLayout(label: string, command: RexCommand) {
  return {
    block: {
      flavor: "com.superlogical.terminal.shell",
      label,
      options: { shell: "none", command: command.argv, cwd: command.cwd },
    },
  };
}

/**
 * Rex's control API through its CLI. `TR3X_REX_SERVER` points it at another
 * server, e.g. a scratch one in tests, and turns off autostart.
 */
export function makeRexClient(env: NodeJS.ProcessEnv) {
  const cli = resolveRexCli(env);
  const server = env.TR3X_REX_SERVER?.trim();
  const global = server ? ["-S", server, "--autostart=false"] : [];
  const requireCli = () => {
    if (cli === undefined) {
      throw new Error(
        "The Rex CLI isn't on PATH or in /Applications. Install Rex, or set TR3X_REX_BIN.",
      );
    }
    return cli;
  };

  const call = (method: string, args: Record<string, unknown>): Promise<unknown> =>
    new Promise((resolve, reject) => {
      NodeChildProcess.execFile(
        requireCli(),
        [...global, "api", "call", method, JSON.stringify(args)],
        { timeout: 15_000, encoding: "utf8" },
        (error, stdout, stderr) => {
          if (error) {
            const detail = stderr.trim().split("\n")[0] || error.message;
            reject(new Error(`rex ${method} failed: ${detail}`));
            return;
          }
          try {
            resolve(stdout.trim() ? JSON.parse(stdout) : {});
          } catch (cause) {
            reject(new Error(`rex ${method} printed something other than JSON.`, { cause }));
          }
        },
      );
    });

  return {
    listSessions: async () =>
      decodeSessionList(await call("session.list", {})).sessions.map((session) => ({
        sessionId: session.session_id,
        label: session.label,
      })),
    /** Every block placed in `sessionId`'s windows, floating layers included. */
    listBlocks: async (sessionId: string): Promise<ReadonlyArray<RexBlock>> =>
      (
        decodeSessionView(await call("session.view", { session_id: sessionId })).windows ?? []
      ).flatMap((window) =>
        window.layers.flatMap((layer) =>
          layer.blocks.map((block) => ({
            sessionId,
            windowId: window.window_id,
            blockId: block.block_id,
            label: block.label,
          })),
        ),
      ),
    /** A new session whose one window runs `command` in a block labelled `blockLabel`. */
    createSession: async (input: {
      readonly label: string;
      readonly windowLabel: string;
      readonly blockLabel: string;
      readonly command: RexCommand;
    }): Promise<RexPlacement> => {
      const created = decodeCreatedSession(
        await call("session.create", {
          label: input.label,
          initial_windows: [
            {
              window_label: input.windowLabel,
              layout: terminalLayout(input.blockLabel, input.command),
            },
          ],
        }),
      );
      const window = created.initial_windows[0];
      const blockId = window?.block_ids[0];
      if (window === undefined || blockId === undefined) {
        throw new Error("rex session.create returned no window.");
      }
      return { sessionId: created.session_id, windowId: window.window_id, blockId };
    },
    createWindow: async (input: {
      readonly sessionId: string;
      readonly windowLabel: string;
      readonly blockLabel: string;
      readonly command: RexCommand;
      readonly focus: boolean;
    }): Promise<RexPlacement> => {
      const created = decodeCreatedWindow(
        await call("session.new_window", {
          session_id: input.sessionId,
          window_label: input.windowLabel,
          focus: input.focus,
          layout: terminalLayout(input.blockLabel, input.command),
        }),
      );
      const blockId = created.block_ids[0];
      if (blockId === undefined) throw new Error("rex session.new_window returned no block.");
      return { sessionId: input.sessionId, windowId: created.window_id, blockId };
    },
    setWindowLabel: (sessionId: string, windowId: string, label: string) =>
      call("session.set_window_label", { session_id: sessionId, window_id: windowId, label }),
    focusWindow: (sessionId: string, windowId: string) =>
      call("session.focus_window", { session_id: sessionId, window_id: windowId }),
    closeWindow: (sessionId: string, windowId: string) =>
      call("session.close_window", { session_id: sessionId, window_id: windowId }),
    /** Asks a Rex app to show a session. The app takes this from the CLI only with Remote Control on. */
    selectSession: (clientId: string, sessionId: string) =>
      new Promise<void>((resolve, reject) => {
        NodeChildProcess.execFile(
          requireCli(),
          [...global, "-C", clientId, "do", "session.select", `session_id=${sessionId}`],
          { timeout: 15_000 },
          (error) => (error ? reject(error) : resolve()),
        );
      }),
    /**
     * Streams each `rex events --all --json` line into `onLine` until `signal` aborts or the
     * stream ends, e.g. when the Rex server stops. `started` resolves once the
     * stream is attached, so no later change is missed; `ended` once `rex` exits.
     */
    streamEvents: (onLine: (line: string) => void, signal: AbortSignal) => {
      const child = NodeChildProcess.spawn(requireCli(), [...global, "events", "--all", "--json"], {
        stdio: ["ignore", "pipe", "ignore"],
      });
      const stop = () => child.kill("SIGTERM");
      signal.addEventListener("abort", stop, { once: true });
      const ended = new Promise<void>((resolve) => {
        child.once("error", () => resolve());
        child.once("close", () => resolve());
      }).then(() => signal.removeEventListener("abort", stop));
      let markStarted = () => {};
      const started = new Promise<void>((resolve) => (markStarted = resolve));
      NodeReadline.createInterface({ input: child.stdout }).on("line", (line) => {
        markStarted();
        onLine(line);
      });
      // Any CLI call shows up on the stream as a client joining, so ping until one does.
      const attached = { done: false };
      void Promise.race([started, ended]).then(() => (attached.done = true));
      void (async () => {
        while (!attached.done) {
          await call("server.status", {}).catch(() => {});
          await new Promise((resolve) => setTimeout(resolve, 200));
        }
      })();
      return { started: Promise.race([started, ended]), ended };
    },
  };
}

export type RexClient = ReturnType<typeof makeRexClient>;
