// @effect-diagnostics nodeBuiltinImport:off globalTimers:off -- Real Rex processes for tests, outside any Effect runtime.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeReadline from "node:readline";

import { makeRexClient, resolveRexCli } from "../rex/rex.ts";

const STOP_MS = 10_000;

export interface ScratchRex {
  /** The `-S` address of this server, for `TR3X_REX_SERVER`. */
  readonly server: string;
  /** Runs the Rex CLI against this server and returns its stdout. */
  readonly rex: (...args: ReadonlyArray<string>) => string;
  /** Runs `rex api call` against this server and returns the decoded result. */
  readonly call: (method: string, args: Record<string, unknown>) => unknown;
  /** Every block labelled in every session: `[label, sessionId, windowId, windowLabel]`. */
  readonly labelledBlocks: () => ReadonlyArray<{
    readonly label: string;
    readonly blockId: string;
    readonly sessionId: string;
    readonly sessionLabel: string;
    readonly windowId: string;
    readonly windowLabel: string;
  }>;
  /** Resolves once the program in `blockId` has reported a status (OSC 7501), as a started view does. */
  readonly waitForProgramStatus: (sessionId: string, blockId: string) => Promise<void>;
  /** Stops the server, which ends every program in its blocks. Safe to call more than once. */
  readonly stop: () => Promise<void>;
}

/**
 * A private Rex server on its own socket, with an empty config, so a test
 * never sees or touches the user's Rex sessions.
 */
export async function startScratchRex(dir: string): Promise<ScratchRex> {
  const cli = resolveRexCli(process.env);
  if (cli === undefined) throw new Error("These tests need the Rex CLI; install Rex.");
  NodeFS.writeFileSync(NodePath.join(dir, "rex-init.lua"), "");
  const server = `unix://${NodePath.join(dir, "rex.sock")}`;
  const child = NodeChildProcess.spawn(
    cli,
    [
      "server",
      "run",
      "--listen",
      server,
      "--state-dir",
      NodePath.join(dir, "rex-state"),
      "--config",
      NodePath.join(dir, "rex-init.lua"),
      "--log-format",
      "json",
    ],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  const closed = new Promise<void>((resolve) => child.once("close", () => resolve()));
  // Rex logs to stderr; stdout carries nothing we need.
  child.stdout.resume();
  const ready = new Promise<void>((resolve, reject) => {
    const lines = NodeReadline.createInterface({ input: child.stderr });
    lines.on("line", (line) => {
      if (line.includes("Rex server ready")) resolve();
    });
    void closed.then(() => reject(new Error("The scratch Rex server exited before it was ready.")));
  });
  let stopping: Promise<void> | undefined;
  const stop = () =>
    (stopping ??= (async () => {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGTERM");
        const timer = setTimeout(() => child.kill("SIGKILL"), STOP_MS);
        await closed;
        clearTimeout(timer);
      }
    })());
  await ready.catch(async (error: unknown) => {
    await stop();
    throw error;
  });

  const rex = (...args: ReadonlyArray<string>) =>
    NodeChildProcess.execFileSync(cli, ["-S", server, "--autostart=false", ...args], {
      encoding: "utf8",
      timeout: 15_000,
    });
  const call = (method: string, args: Record<string, unknown>): unknown =>
    JSON.parse(rex("api", "call", method, JSON.stringify(args)));

  return {
    server,
    rex,
    call,
    labelledBlocks: () => {
      const { sessions } = call("session.list", {}) as {
        sessions: Array<{ session_id: string; label: string }>;
      };
      return sessions.flatMap((session) => {
        const view = call("session.view", { session_id: session.session_id }) as {
          windows?: Array<{
            window_id: string;
            label: string;
            layers: Array<{ blocks: Array<{ block_id: string; label: string }> }>;
          }>;
        };
        return (view.windows ?? []).flatMap((window) =>
          window.layers.flatMap((layer) =>
            layer.blocks.map((block) => ({
              label: block.label,
              blockId: block.block_id,
              sessionId: session.session_id,
              sessionLabel: session.label,
              windowId: window.window_id,
              windowLabel: window.label,
            })),
          ),
        );
      });
    },
    waitForProgramStatus: async (sessionId, blockId) => {
      const stopWatching = new AbortController();
      let reported = () => {};
      const reportedNow = new Promise<void>((resolve) => (reported = resolve));
      const stream = makeRexClient({ ...process.env, TR3X_REX_SERVER: server }).streamEvents(
        (line) => {
          if (
            line.includes(`"block_id":"${blockId}"`) &&
            line.includes('"program_status_changed"')
          ) {
            reported();
          }
        },
        stopWatching.signal,
      );
      try {
        // Attached from here on, so a status reported before this is in the block already.
        await stream.started;
        const current = JSON.parse(
          rex("block", "call", "-s", sessionId, "-b", blockId, "program_status"),
        ) as { records?: ReadonlyArray<unknown> };
        if ((current.records?.length ?? 0) === 0) await Promise.race([reportedNow, stream.ended]);
      } finally {
        stopWatching.abort();
        await stream.ended;
      }
    },
    stop,
  };
}
