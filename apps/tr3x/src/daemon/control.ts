// @effect-diagnostics nodeBuiltinImport:off globalTimers:off -- tr3x runs on Bun outside the Effect runtime.
import * as NodeFS from "node:fs";
import * as NodeNet from "node:net";
import * as NodePath from "node:path";
import * as Schema from "effect/Schema";

import { describeError } from "../redact.ts";
import { ensurePrivateDir } from "./daemonState.ts";

/**
 * The daemon's control socket: one JSON request per connection, one JSON reply.
 * The socket is 0600 in a 0700 directory, so only this user can ask; that
 * user could mint sessions with `t3 auth` anyway.
 */
export const ControlRequest = Schema.Union([
  Schema.Struct({ op: Schema.Literal("new"), cwd: Schema.String }),
  Schema.Struct({ op: Schema.Literal("open"), threadId: Schema.String }),
  Schema.Struct({ op: Schema.Literal("next-blocked") }),
  Schema.Struct({ op: Schema.Literal("threads") }),
  /** A fresh bearer for a tab's view, before its current one expires. */
  Schema.Struct({ op: Schema.Literal("bearer"), threadId: Schema.String }),
]);
export type ControlRequest = typeof ControlRequest.Type;

const ControlReply = Schema.Union([
  Schema.Struct({ ok: Schema.Literal(true), result: Schema.Unknown }),
  Schema.Struct({ ok: Schema.Literal(false), error: Schema.String }),
]);

const decodeRequest = Schema.decodeUnknownSync(Schema.fromJsonString(ControlRequest));
const decodeReply = Schema.decodeUnknownSync(Schema.fromJsonString(ControlReply));

/** The socket answers, so another daemon is listening on it. */
function isListening(path: string): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = NodeNet.connect(path);
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("error", () => resolve(false));
  });
}

export interface ControlServer {
  readonly close: () => Promise<void>;
}

/**
 * Listens on `path`, replacing a socket a crashed daemon left behind. Throws
 * when another daemon answers on it.
 */
export async function startControlServer(
  path: string,
  handle: (request: ControlRequest) => Promise<unknown>,
): Promise<ControlServer> {
  ensurePrivateDir(NodePath.dirname(path));
  if (NodeFS.existsSync(path)) {
    if (await isListening(path)) {
      throw new Error(`Another tr3x daemon is listening on ${path}.`);
    }
    NodeFS.rmSync(path, { force: true });
  }
  const server = NodeNet.createServer((socket) => {
    let input = "";
    socket.setEncoding("utf8");
    socket.on("error", () => {});
    socket.on("data", (chunk) => {
      input += chunk;
      const end = input.indexOf("\n");
      if (end === -1) return;
      socket.removeAllListeners("data");
      const reply = (body: typeof ControlReply.Type) => socket.end(`${JSON.stringify(body)}\n`);
      let request: ControlRequest;
      try {
        request = decodeRequest(input.slice(0, end));
      } catch {
        reply({ ok: false, error: "tr3x daemon: unknown request." });
        return;
      }
      handle(request).then(
        (result) => reply({ ok: true, result: result ?? null }),
        (error: unknown) => reply({ ok: false, error: describeError(error) }),
      );
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(path, () => {
      server.off("error", reject);
      resolve();
    });
  });
  NodeFS.chmodSync(path, 0o600);
  return {
    close: () =>
      new Promise((resolve) => {
        server.close(() => resolve());
        NodeFS.rmSync(path, { force: true });
      }),
  };
}

/** The daemon isn't listening. */
export class DaemonNotRunningError extends Error {}

/** Sends one request to the daemon and returns its result, or throws its error. */
export function controlRequest(
  path: string,
  request: ControlRequest,
  timeoutMs = 60_000,
): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const socket = NodeNet.connect(path);
    let output = "";
    socket.setEncoding("utf8");
    socket.setTimeout(timeoutMs, () => {
      socket.destroy();
      reject(new Error("The tr3x daemon didn't answer in time."));
    });
    socket.once("connect", () => socket.write(`${JSON.stringify(request)}\n`));
    socket.on("data", (chunk) => (output += chunk));
    socket.once("error", (error: NodeJS.ErrnoException) => {
      reject(
        error.code === "ENOENT" || error.code === "ECONNREFUSED"
          ? new DaemonNotRunningError("No tr3x daemon is running. Start it with `tr3x daemon`.")
          : error,
      );
    });
    socket.once("end", () => {
      try {
        const reply = decodeReply(output.trim());
        if (reply.ok) resolve(reply.result);
        else reject(new Error(reply.error));
      } catch {
        reject(new Error("The tr3x daemon sent a reply tr3x can't read."));
      }
    });
  });
}
