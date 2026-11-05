// @effect-diagnostics nodeBuiltinImport:off -- Real files and processes, outside any Effect runtime.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { afterEach, describe, expect, it } from "vite-plus/test";

import { controlRequest, startControlServer } from "./control.ts";

const cleanup: Array<() => Promise<void> | void> = [];
afterEach(async () => {
  for (const step of cleanup.splice(0).reverse()) await step();
});

function tempDir(): string {
  const dir = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "tr3x-control-"));
  cleanup.push(() => NodeFS.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

describe("the control socket", () => {
  it("answers on a 0600 socket, replaces a crashed daemon's, and refuses to share a live one", async () => {
    const path = NodePath.join(tempDir(), "state", "daemon.sock");
    NodeFS.mkdirSync(NodePath.dirname(path), { mode: 0o700 });
    // A daemon killed with SIGKILL leaves its socket file behind, with nobody listening.
    const crashed = NodeChildProcess.spawn(
      process.execPath,
      [
        "-e",
        "require('net').createServer().listen(process.argv[1], () => console.log('ready'))",
        path,
      ],
      { stdio: ["ignore", "pipe", "ignore"] },
    );
    await new Promise((resolve) => crashed.stdout.once("data", resolve));
    crashed.kill("SIGKILL");
    await new Promise((resolve) => crashed.once("close", resolve));
    expect(NodeFS.statSync(path).isSocket()).toBe(true);

    const server = await startControlServer(path, async (request) =>
      request.op === "open" ? { threadId: request.threadId } : null,
    );
    cleanup.push(() => server.close());
    expect(NodeFS.statSync(path).mode & 0o777).toBe(0o600);
    expect(await controlRequest(path, { op: "open", threadId: "t1" })).toEqual({ threadId: "t1" });
    await expect(startControlServer(path, async () => null)).rejects.toThrow(
      /Another tr3x daemon/u,
    );
  });

  it("says when no daemon is running", async () => {
    await expect(
      controlRequest(NodePath.join(tempDir(), "missing.sock"), { op: "next-blocked" }),
    ).rejects.toThrow("No tr3x daemon is running");
  });
});
