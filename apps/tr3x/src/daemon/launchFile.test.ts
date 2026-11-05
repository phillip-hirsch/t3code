// @effect-diagnostics nodeBuiltinImport:off -- Real files, outside any Effect runtime.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { afterEach, describe, expect, it } from "vite-plus/test";

import {
  consumeLaunchFile,
  type LaunchFile,
  newLaunchFilePath,
  writeLaunchFile,
} from "./launchFile.ts";

const cleanup: Array<() => Promise<void> | void> = [];
afterEach(async () => {
  for (const step of cleanup.splice(0).reverse()) await step();
});

function tempDir(): string {
  const dir = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "tr3x-launch-"));
  cleanup.push(() => NodeFS.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

const LAUNCH: LaunchFile = {
  version: 1,
  server: {
    environmentId: "env-1",
    label: "tr3x",
    httpBaseUrl: "http://127.0.0.1:43111",
    wsBaseUrl: "ws://127.0.0.1:43111",
  },
  threadId: "thread-1",
  bearer: { token: "secret-token", expiresAt: "2026-10-28T21:00:00.000Z" },
  controlSocket: "/tmp/daemon.sock",
};

describe("launch files", () => {
  it("are private, written once, and work once", () => {
    const dir = NodePath.join(tempDir(), "launch");
    const path = newLaunchFilePath(dir);
    writeLaunchFile(path, LAUNCH);
    expect(NodeFS.statSync(dir).mode & 0o777).toBe(0o700);
    expect(NodeFS.statSync(path).mode & 0o777).toBe(0o600);
    expect(() => writeLaunchFile(path, LAUNCH)).toThrow(/EEXIST/u);

    expect(consumeLaunchFile(path)).toEqual(LAUNCH);
    expect(NodeFS.existsSync(path)).toBe(false);
    expect(() => consumeLaunchFile(path)).toThrow(/used or expired/u);
  });

  it("refuses a file others can read, or a symlink", () => {
    const dir = NodePath.join(tempDir(), "launch");
    const readable = newLaunchFilePath(dir);
    writeLaunchFile(readable, LAUNCH);
    NodeFS.chmodSync(readable, 0o644);
    expect(() => consumeLaunchFile(readable)).toThrow(/accessible to other users/u);

    const target = newLaunchFilePath(dir);
    writeLaunchFile(target, LAUNCH);
    const link = newLaunchFilePath(dir);
    NodeFS.symlinkSync(target, link);
    expect(() => consumeLaunchFile(link)).toThrow(/symlink/u);
  });
});
