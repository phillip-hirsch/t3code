// @effect-diagnostics nodeBuiltinImport:off -- Drives real tr3x, T3 server and Rex processes, outside any Effect runtime.
import * as NodeChildProcess from "node:child_process";
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import {
  AuthOrchestrationOperateScope,
  AuthOrchestrationReadScope,
  EnvironmentId,
  MessageId,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
} from "@t3tools/contracts";
import { afterEach, describe, expect, it } from "vite-plus/test";

import { resolveTr3xPaths } from "../paths.ts";
import { CLI, spawnBun, waitForLine } from "../testing/bunProcess.ts";
import { startDaemon } from "../testing/daemonFixture.ts";
import { installFakeProviders } from "../testing/fakeProviders.ts";
import { startScratchRex } from "../testing/rexFixture.ts";
import { createViewConnection } from "../view/connection.ts";
import { controlRequest } from "./control.ts";
import { readDaemonState } from "./daemonState.ts";
import { type LabelId, parseTabBlockLabel } from "./tabs.ts";
import { issueLocalSession, listLocalSessions } from "./localSession.ts";

// Runs whatever happens to the test, last step first: daemons, then Rex (which
// ends the views in its tabs), then the directory they ran in.
const cleanup: Array<() => Promise<void> | void> = [];
afterEach(async () => {
  for (const step of cleanup.splice(0).reverse()) await step();
});

const NODE_ENV = { TR3X_NODE_BIN: process.execPath };

async function setUp() {
  const dir = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "tr3x-tabs-"));
  cleanup.push(() => NodeFS.rmSync(dir, { recursive: true, force: true }));
  const home = NodePath.join(dir, "home");
  const work = NodePath.join(dir, "work");
  NodeFS.mkdirSync(work);
  installFakeProviders(home, "Hello from the fake provider.");
  const rex = await startScratchRex(dir);
  cleanup.push(() => rex.stop());
  const env = { TR3X_REX_SERVER: rex.server };
  const daemon = () => {
    const fixture = startDaemon(home, env);
    cleanup.push(async () => void (await fixture.stop()));
    return fixture;
  };
  /** Runs a `tr3x` command against the daemon and returns what it printed. */
  const tr3x = async (...args: ReadonlyArray<string>) => {
    const run = spawnBun([CLI, ...args], { ...env, TR3X_HOME: home }, (step) => cleanup.push(step));
    const code = await run.exited;
    expect(code, run.output()).toBe(0);
    return run.output();
  };
  /** The tr3x tabs in the scratch Rex, with the thread and project their labels name. */
  const tabs = () =>
    rex.labelledBlocks().flatMap((block) => {
      const parsed = parseTabBlockLabel(block.label);
      if (parsed === undefined) return [];
      // Short ids show as themselves; a digest stands for an id too long for a label.
      const id = (part: LabelId) => (part.kind === "id" ? part.id : `#${part.digest}`);
      return [
        {
          ...block,
          threadId: id(parsed.thread),
          projectId: parsed.project === undefined ? undefined : id(parsed.project),
        },
      ];
    });
  const tabSessions = async (threadId: string) =>
    (await listLocalSessions(home, NODE_ENV)).filter(
      (session) => session.subject === `tr3x-tab:${threadId}`,
    );
  /** A client of the daemon's server, as web would be: its own session, through the T3 API. */
  const apiConnection = async () => {
    const state = readDaemonState(resolveTr3xPaths({ TR3X_HOME: home }).daemonStatePath)!;
    const bearer = await issueLocalSession({
      baseDir: home,
      scopes: [AuthOrchestrationReadScope, AuthOrchestrationOperateScope],
      ttl: "5m",
      label: "tr3x test",
      env: NODE_ENV,
    });
    const connection = createViewConnection({
      environmentId: EnvironmentId.make(state.environmentId),
      label: "tr3x test",
      httpBaseUrl: state.httpBaseUrl,
      wsBaseUrl: state.wsBaseUrl,
      bearerToken: async () => bearer.token,
    });
    cleanup.push(() => connection.dispose());
    // Commands need a live connection; the shell subscription holds one open.
    cleanup.push(connection.registry.mount(connection.shellStateAtom));
    await connection.shellSnapshot(AbortSignal.timeout(20_000));
    return connection;
  };
  return { dir, home, work, rex, daemon, tr3x, tabs, tabSessions, apiConnection };
}

/** Command lines of every process whose arguments mention `text`. */
function processesMentioning(text: string): ReadonlyArray<string> {
  return NodeChildProcess.execFileSync("ps", ["-A", "-ww", "-o", "args="], { encoding: "utf8" })
    .split("\n")
    .filter((line) => line.includes(text));
}

describe("tr3x daemon tabs", () => {
  it("opens, relabels, hides, reopens and closes a thread's tab, across a daemon restart", async () => {
    const { home, work, rex, daemon, tr3x, tabs, tabSessions, apiConnection } = await setUp();
    const paths = resolveTr3xPaths({ TR3X_HOME: home, XDG_STATE_HOME: `${home}-state` });
    const first = daemon();
    await waitForLine(first.daemon, /found 0 thread tab\(s\) in Rex/u);

    // New Thread opens a tab, labelled for the thread, in the project's session.
    const opened = JSON.parse(await tr3x("new", "--cwd", work)) as {
      threadId: string;
      sessionId: string;
      windowId: string;
    };
    const { threadId } = opened;
    await waitForLine(first.daemon, new RegExp(`handed thread ${threadId}'s tab its session`, "u"));
    const [tab] = tabs();
    expect(tabs()).toEqual([
      expect.objectContaining({
        threadId,
        projectId: expect.any(String),
        sessionId: opened.sessionId,
        sessionLabel: "work",
        windowId: opened.windowId,
        windowLabel: "New thread",
      }),
    ]);
    await rex.waitForProgramStatus(tab!.sessionId, tab!.blockId);

    // The view took its bearer from the one-shot launch file: only the file's path is in argv.
    expect(NodeFS.readdirSync(paths.launchDir)).toEqual([]);
    const launched = processesMentioning(paths.launchDir);
    expect(launched.length).toBeGreaterThan(0);
    for (const line of launched) {
      expect(line).toMatch(/env TR3X_LAUNCH=\S+\/[\w-]+\.json \S+ \S+\/cli\.ts view$/u);
    }
    for (const line of processesMentioning(home)) expect(line).not.toMatch(/token|bearer|eyJ/iu);
    expect(await tabSessions(threadId)).toHaveLength(1);

    // The first message launches the thread; its title becomes the tab's label.
    rex.rex("send", "-s", tab!.sessionId, "-b", tab!.blockId, "Say hello");
    rex.rex("send-key", "-s", tab!.sessionId, "-b", tab!.blockId, "enter");
    await waitForLine(first.daemon, new RegExp(`relabelled thread ${threadId}'s tab`, "u"));
    expect(tabs()).toEqual([expect.objectContaining({ windowLabel: "Say hello" })]);

    // Closing the tab hides the thread, without archiving it, and revokes the tab's session.
    let mark = first.daemon.output().length;
    rex.rex("window", "close", "-s", tab!.sessionId, tab!.windowId);
    await waitForLine(
      first.daemon,
      new RegExp(`revoked 1 session\\(s\\) for thread ${threadId}`, "u"),
      mark,
    );
    expect(first.daemon.output().slice(mark)).toContain("hiding the thread");
    expect(tabs()).toEqual([]);
    expect(await tabSessions(threadId)).toEqual([]);
    // A closed tab's view can't get a fresh bearer either.
    await expect(
      controlRequest(paths.controlSocketPath, { op: "bearer", threadId }),
    ).rejects.toThrow("That thread has no tab.");

    // Open brings it back in a new tab with a new session.
    mark = first.daemon.output().length;
    await tr3x("open", threadId);
    await waitForLine(
      first.daemon,
      new RegExp(`handed thread ${threadId}'s tab its session`, "u"),
      mark,
    );
    const [reopened] = tabs();
    expect(reopened).toEqual(expect.objectContaining({ threadId, windowLabel: "Say hello" }));
    expect(reopened!.blockId).not.toBe(tab!.blockId);

    // A daemon killed outright, then started again, finds the tab instead of opening another.
    first.daemon.child.kill("SIGKILL");
    await first.stop();
    const second = daemon();
    await waitForLine(second.daemon, /found 1 thread tab\(s\) in Rex/u);
    expect(tabs()).toEqual([expect.objectContaining({ blockId: reopened!.blockId })]);

    // An archive no daemon saw (this one is suspended) still closes the tab once one starts.
    const connection = await apiConnection();
    second.daemon.child.kill("SIGSTOP");
    expect(await connection.archiveThread({ threadId: ThreadId.make(threadId) })).toEqual({
      ok: true,
    });
    second.daemon.child.kill("SIGKILL");
    await second.stop();
    const third = daemon();
    await waitForLine(
      third.daemon,
      new RegExp(`revoked 1 session\\(s\\) for thread ${threadId}`, "u"),
    );
    expect(third.daemon.output()).toContain(
      `closing thread ${threadId}'s tab: the thread was archived`,
    );
    expect(tabs()).toEqual([]);
    expect(await tabSessions(threadId)).toEqual([]);
  });

  it("opens a background tab for a thread that blocks without one, and Next Blocked goes there", async () => {
    const { dir, work, daemon, tr3x, tabs, apiConnection } = await setUp();
    const owner = daemon();
    await waitForLine(owner.daemon, /found 0 thread tab\(s\) in Rex/u);
    expect(JSON.parse(await tr3x("next-blocked"))).toBeNull();

    // A thread started elsewhere, e.g. in web, on a provider that asks before running a command.
    const connection = await apiConnection();
    const projectId = ProjectId.make(NodeCrypto.randomUUID());
    expect(
      await connection.createProject({ projectId, title: "work", workspaceRoot: work }),
    ).toEqual({ ok: true });
    const threadId = ThreadId.make(NodeCrypto.randomUUID());
    const modelSelection = { instanceId: ProviderInstanceId.make("grokB"), model: "grok-4.6" };
    expect(
      await connection.startTurn({
        threadId,
        message: {
          messageId: MessageId.make(NodeCrypto.randomUUID()),
          role: "user",
          text: "Run the tool",
          attachments: [],
        },
        modelSelection,
        titleSeed: "Run the tool",
        runtimeMode: "approval-required",
        interactionMode: "default",
        bootstrap: {
          createThread: {
            projectId,
            title: "Run the tool",
            modelSelection,
            runtimeMode: "approval-required",
            interactionMode: "default",
            branch: null,
            worktreePath: null,
            createdAt: "2026-10-28T00:00:00.000Z",
          },
        },
      }),
    ).toEqual({ ok: true });

    await waitForLine(owner.daemon, new RegExp(`handed thread ${threadId}'s tab its session`, "u"));
    expect(tabs()).toEqual([expect.objectContaining({ threadId, sessionLabel: "work" })]);
    const next = JSON.parse(await tr3x("next-blocked")) as { threadId: string; sessionId: string };
    expect(next.threadId).toBe(threadId);
    expect(tabs()).toHaveLength(1);

    // Another project with the same title gets a session of its own.
    const otherWork = NodePath.join(dir, "elsewhere", "work");
    NodeFS.mkdirSync(otherWork, { recursive: true });
    const other = JSON.parse(await tr3x("new", "--cwd", otherWork)) as { sessionId: string };
    expect(other.sessionId).not.toBe(next.sessionId);

    // Archiving through the T3 API closes the tab and revokes its session.
    let mark = owner.daemon.output().length;
    expect(await connection.archiveThread({ threadId })).toEqual({ ok: true });
    await waitForLine(
      owner.daemon,
      new RegExp(`revoked 1 session\\(s\\) for thread ${threadId}`, "u"),
      mark,
    );
    expect(tabs().map((tab) => tab.threadId)).not.toContain(threadId);

    // Open Thread lists it as archived; opening it unarchives it into a new tab.
    expect(await tr3x("pick")).toContain(`${threadId}\tRun the tool\twork\tarchived\n`);
    mark = owner.daemon.output().length;
    expect(JSON.parse(await tr3x("open", threadId))).toEqual(
      expect.objectContaining({ threadId, sessionId: next.sessionId }),
    );
    await waitForLine(
      owner.daemon,
      new RegExp(`handed thread ${threadId}'s tab its session`, "u"),
      mark,
    );
    expect(tabs().map((tab) => tab.threadId)).toContain(threadId);
  });

  it("lists the older threads the shell leaves out, and keeps each project's session after a restart", async () => {
    const { dir, work, daemon, tr3x, tabs, apiConnection } = await setUp();
    const first = daemon();
    await waitForLine(first.daemon, /found 0 thread tab\(s\) in Rex/u);

    // Project B: a new thread's tab, before the first message that would create the thread.
    const otherWork = NodePath.join(dir, "elsewhere", "work");
    NodeFS.mkdirSync(otherWork, { recursive: true });
    const draft = JSON.parse(await tr3x("new", "--cwd", otherWork)) as {
      threadId: string;
      sessionId: string;
    };
    await waitForLine(
      first.daemon,
      new RegExp(`handed thread ${draft.threadId}'s tab its session`, "u"),
    );

    // Project A, also titled "work", with an id too long for a label: more settled
    // threads than the shell keeps (25).
    const connection = await apiConnection();
    const projectId = ProjectId.make(`mcp:${"project ".repeat(12)}${NodeCrypto.randomUUID()}`);
    expect(
      await connection.createProject({ projectId, title: "work", workspaceRoot: work }),
    ).toEqual({ ok: true });
    const settled: string[] = [];
    for (let index = 0; index < 27; index += 1) {
      // Ids too long for a Rex label, like app-owned delegated threads have.
      const threadId = ThreadId.make(`delegated:${"é".repeat(30)}:${NodeCrypto.randomUUID()}`);
      expect(
        await connection.createThread({
          threadId,
          projectId,
          title: `Settled ${index}`,
          modelSelection: { instanceId: ProviderInstanceId.make("grok"), model: "grok-4.6" },
          runtimeMode: "full-access",
          interactionMode: "default",
          branch: null,
          worktreePath: null,
        }),
      ).toEqual({ ok: true });
      expect(await connection.settleThread({ threadId })).toEqual({ ok: true });
      settled.push(threadId);
    }
    // The oldest one, which a fresh shell leaves out, has a tab.
    const oldest = settled[0]!;
    const opened = JSON.parse(await tr3x("open", oldest)) as { sessionId: string };
    await waitForLine(first.daemon, new RegExp(`handed thread ${oldest}'s tab its session`, "u"));
    expect(opened.sessionId).not.toBe(draft.sessionId);

    first.daemon.child.kill("SIGKILL");
    await first.stop();
    const second = daemon();
    await waitForLine(second.daemon, /found 2 thread tab\(s\) in Rex/u);

    // A fresh shell leaves some threads out, that tab's among them; Open Thread still lists them all.
    const fresh = await (await apiConnection()).shellSnapshot(AbortSignal.timeout(20_000));
    expect(fresh.threads.map((thread) => thread.id as string)).not.toContain(oldest);
    const listing = await tr3x("pick");
    for (const threadId of settled) expect(listing).toContain(threadId);

    // Each project's new threads still go to its own session, whatever the title.
    const inA = JSON.parse(await tr3x("new", "--cwd", work)) as { sessionId: string };
    expect(inA.sessionId).toBe(opened.sessionId);
    const inB = JSON.parse(await tr3x("new", "--cwd", otherWork)) as { sessionId: string };
    expect(inB.sessionId).toBe(draft.sessionId);
    expect(new Set(tabs().map((tab) => tab.sessionLabel))).toEqual(new Set(["work"]));
  });
});
