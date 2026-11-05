// @effect-diagnostics nodeBuiltinImport:off globalTimers:off globalDate:off -- tr3x runs on Bun outside the Effect runtime.
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as Schema from "effect/Schema";

import { assertPrivate, ensurePrivateDir, UnsafeDaemonStateError } from "./daemonState.ts";

/**
 * What the daemon hands a tab's `tr3x view`: the server, the thread, and a
 * bearer for it. Rex passes no environment to a block, and argv shows in `ps`,
 * so the bearer travels in a one-shot file whose path is all argv carries.
 */
export const LaunchFile = Schema.Struct({
  version: Schema.Literal(1),
  server: Schema.Struct({
    environmentId: Schema.String,
    label: Schema.String,
    httpBaseUrl: Schema.String,
    wsBaseUrl: Schema.String,
  }),
  threadId: Schema.String,
  /** Set when the tab starts the thread: its first message launches it in this project. */
  newThreadProjectId: Schema.optional(Schema.String),
  bearer: Schema.Struct({ token: Schema.String, expiresAt: Schema.String }),
  /** The daemon's control socket, where the view asks for a fresh bearer before this one expires. */
  controlSocket: Schema.String,
});
export type LaunchFile = typeof LaunchFile.Type;

const encodeLaunchFile = Schema.encodeSync(Schema.fromJsonString(LaunchFile));
const decodeLaunchFile = Schema.decodeUnknownSync(Schema.fromJsonString(LaunchFile));

/** A fresh launch file path in the 0700 `dir`, which this creates if needed. */
export function newLaunchFilePath(dir: string): string {
  ensurePrivateDir(dir);
  return NodePath.join(dir, `${NodeCrypto.randomUUID()}.json`);
}

/** Writes a launch file: 0600, and created exclusively, so nothing already there is reused. */
export function writeLaunchFile(path: string, launch: LaunchFile): void {
  NodeFS.writeFileSync(path, encodeLaunchFile(launch), { mode: 0o600, flag: "wx" });
}

/**
 * Reads a launch file and deletes it, so it works once. Refuses a symlink, or
 * a file or directory that isn't private to this user.
 */
export function consumeLaunchFile(path: string): LaunchFile {
  const dir = NodePath.dirname(path);
  assertPrivate(NodeFS.lstatSync(dir), dir, "directory");
  let fd: number;
  try {
    fd = NodeFS.openSync(path, NodeFS.constants.O_RDONLY | NodeFS.constants.O_NOFOLLOW);
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code === "ELOOP") throw new UnsafeDaemonStateError(`${path} is a symlink.`);
    if (code === "ENOENT") {
      throw new Error(
        "This tab's launch file is gone: it was used or expired. Reopen the thread.",
        {
          cause: error,
        },
      );
    }
    throw error;
  }
  try {
    assertPrivate(NodeFS.fstatSync(fd), path, "file");
    const contents = NodeFS.readFileSync(fd, "utf8");
    NodeFS.rmSync(path, { force: true });
    return decodeLaunchFile(contents);
  } finally {
    NodeFS.closeSync(fd);
  }
}

/** How long a view waits for the daemon to write its launch file, which it does once it has minted the tab's session. */
const LAUNCH_WAIT_MS = 30_000;

/**
 * Waits for the launch file, then consumes it. The daemon opens the tab first
 * and writes the file once it has a session, so the tab never waits on Rex.
 */
export async function waitForLaunchFile(path: string, signal: AbortSignal): Promise<LaunchFile> {
  const deadline = Date.now() + LAUNCH_WAIT_MS;
  while (!NodeFS.existsSync(path)) {
    if (signal.aborted) throw new Error("Stopped.");
    if (Date.now() > deadline) {
      throw new Error("The tr3x daemon didn't hand this tab a session. Reopen the thread.");
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return consumeLaunchFile(path);
}
