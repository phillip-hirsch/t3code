import { describe, expect, it } from "vite-plus/test";

import { parseArgs } from "./args.ts";

describe("parseArgs", () => {
  it("reads each command and refuses anything else", () => {
    expect(parseArgs(["daemon"], "/w")).toEqual({ kind: "daemon" });
    expect(parseArgs(["view", "thread-1"], "/w")).toEqual({ kind: "view", threadId: "thread-1" });
    expect(parseArgs(["view", "--new"], "/w")).toEqual({ kind: "new", cwd: "/w" });
    expect(parseArgs(["view", "--new", "--cwd", "/repo"], "/w")).toEqual({
      kind: "new",
      cwd: "/repo",
    });
    expect(parseArgs(["view"], "/w")).toEqual({ kind: "launch" });
    expect(parseArgs(["new"], "/w")).toEqual({ kind: "tab-new", cwd: "/w" });
    expect(parseArgs(["new", "--cwd", "/repo"], "/w")).toEqual({ kind: "tab-new", cwd: "/repo" });
    expect(parseArgs(["open", "thread-1"], "/w")).toEqual({ kind: "open", threadId: "thread-1" });
    expect(parseArgs(["next-blocked"], "/w")).toEqual({ kind: "next-blocked" });
    expect(parseArgs(["setup", "rex"], "/w")).toEqual({ kind: "setup-rex" });
    expect(parseArgs(["pick"], "/w")).toEqual({ kind: "pick", clientId: undefined });
    expect(parseArgs(["pick", "--client", "c1"], "/w")).toEqual({ kind: "pick", clientId: "c1" });
    expect(parseArgs(["pick", "c1"], "/w")).toBeNull();
    expect(parseArgs(["new", "/repo"], "/w")).toBeNull();
    expect(parseArgs(["open"], "/w")).toBeNull();
    expect(parseArgs(["open", "--all"], "/w")).toBeNull();
    expect(parseArgs(["setup"], "/w")).toBeNull();
    expect(parseArgs(["view", "--cwd", "/repo"], "/w")).toBeNull();
    expect(parseArgs(["view", "a", "b"], "/w")).toBeNull();
    expect(parseArgs(["doctor", "--verbose"], "/w")).toBeNull();
  });
});
