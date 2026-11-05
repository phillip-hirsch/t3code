// @effect-diagnostics nodeBuiltinImport:off globalTimers:off globalDate:off -- The daemon runs on Bun outside the Effect runtime.
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import {
  AuthOrchestrationOperateScope,
  AuthOrchestrationReadScope,
  EnvironmentId,
  type OrchestrationProjectShell as ProjectShell,
  type OrchestrationV2ShellSnapshot,
  ThreadId,
} from "@t3tools/contracts";
import * as Option from "effect/Option";

import type { Tr3xPaths } from "../paths.ts";
import { describeError } from "../redact.ts";
import {
  makeRexClient,
  parseRexEvent,
  type RexBlock,
  type RexEvent,
  resolveRexCli,
} from "../rex/rex.ts";
import { createViewConnection } from "../view/connection.ts";
import { resolveProject } from "../view/resolveProject.ts";
import { sessionBearer } from "../view/sessionBearer.ts";
import type { ControlRequest } from "./control.ts";
import { type LaunchFile, newLaunchFilePath, writeLaunchFile } from "./launchFile.ts";
import {
  issueLocalSession,
  type IssuedSession,
  listLocalSessions,
  revokeLocalSession,
} from "./localSession.ts";
import {
  blockingRequestId,
  nextBlockedThread,
  planShell,
  rexLabel,
  collectOlderSettled,
  labelDigest,
  OLDER_SETTLED_PAGE,
  parseTabBlockLabel,
  resolveLabelId,
  tabBlockLabel,
  type ThreadShell,
} from "./tabs.ts";

const CLI = NodeURL.fileURLToPath(new URL("../cli.ts", import.meta.url));
const SESSION_TTL = "1h";
const SESSION_REFRESH_MS = 5 * 60_000;
/** A launch file no view has read by then is deleted. */
const LAUNCH_FILE_TTL_MS = 60_000;
const REX_RETRY_MS = 2_000;
const TAB_SUBJECT_PREFIX = "tr3x-tab:";
const DAEMON_SUBJECT = "tr3x-daemon";

export interface TabServiceOptions {
  readonly paths: Tr3xPaths;
  readonly env: NodeJS.ProcessEnv;
  readonly server: LaunchFile["server"];
  readonly log: (line: string) => void;
}

interface Tab {
  readonly threadId: string;
  readonly sessionId: string;
  readonly windowId: string;
  readonly blockId: string;
  label: string;
  lastSeen: ThreadShell | undefined;
  /** Set the moment Rex reports the tab closed, before the queue gets to it. */
  closed: boolean;
}

/** Where `open`, `new` and `next-blocked` put the thread, so the Rex glue can show that session. */
export interface OpenedTab {
  readonly threadId: string;
  readonly sessionId: string;
  readonly windowId: string;
}

export interface ThreadEntry {
  readonly threadId: string;
  readonly title: string;
  readonly project: string;
  readonly open: boolean;
  readonly blocked: boolean;
  readonly archived: boolean;
}

export interface TabService {
  readonly handle: (request: ControlRequest) => Promise<unknown>;
  readonly stop: () => Promise<void>;
}

const tabSubject = (threadId: string) => `${TAB_SUBJECT_PREFIX}${threadId}`;

/**
 * One Rex tab per open thread, one Rex session per project. A tab the user
 * closes hides its thread (never archives it); every closed tab's sessions are
 * revoked. A restarted daemon finds its tabs again by their block labels.
 */
export async function startTabService(options: TabServiceOptions): Promise<TabService> {
  const { paths, env, log } = options;
  const startedAt = Date.now();
  const stopController = new AbortController();
  const { signal } = stopController;
  const rex = resolveRexCli(env) === undefined ? undefined : makeRexClient(env);
  if (rex === undefined) log("Rex CLI not found; threads won't open as tabs. Set TR3X_REX_BIN.");

  const connection = createViewConnection({
    ...options.server,
    environmentId: EnvironmentId.make(options.server.environmentId),
    bearerToken: sessionBearer(
      () =>
        issueLocalSession({
          baseDir: paths.home,
          scopes: [AuthOrchestrationReadScope, AuthOrchestrationOperateScope],
          ttl: SESSION_TTL,
          label: "tr3x daemon",
          subject: DAEMON_SUBJECT,
          env,
        }),
      SESSION_REFRESH_MS,
    ),
  });

  const tabs = new Map<string, Tab>();
  const threadOfBlock = new Map<string, string>();
  const projectSessions = new Map<string, string>();
  /** Threads whose tab the user closed, with the request they were blocked on then. */
  const hidden = new Map<string, string | null>();
  const launchFiles = new Set<string>();
  let lastNextBlocked: string | undefined;
  let mapped = false;

  // Every change to the map runs here, one at a time, so events never interleave.
  let queue: Promise<unknown> = Promise.resolve();
  const serial = <A>(task: () => Promise<A>): Promise<A> => {
    const run = queue.then(task);
    queue = run.catch(() => {});
    return run;
  };

  const shellState = () => connection.registry.get(connection.shellStateAtom);
  const requireShell = (): OrchestrationV2ShellSnapshot => {
    const snapshot = Option.getOrNull(shellState().snapshot);
    if (snapshot === null) throw new Error("The tr3x daemon hasn't loaded threads yet; try again.");
    return snapshot;
  };

  const forget = (tab: Tab) => {
    tab.closed = true;
    tabs.delete(tab.threadId);
    threadOfBlock.delete(tab.blockId);
  };
  const isLive = (tab: Tab) => !tab.closed && tabs.get(tab.threadId) === tab;

  /** Revokes every session minted for the thread's tabs up to `before`, when its tab closed. */
  const revokeTabSessions = async (threadId: string, before: number) => {
    const sessions = await listLocalSessions(paths.home, env);
    const stale = sessions.filter(
      (session) =>
        session.subject === tabSubject(threadId) && Date.parse(session.issuedAt) <= before,
    );
    for (const session of stale) await revokeLocalSession(paths.home, session.sessionId, env);
    log(`revoked ${stale.length} session(s) for thread ${threadId}`);
  };
  /** Threads whose sessions are being revoked, which the leftover sweep leaves alone. */
  const revoking = new Set<string>();
  const revokeLater = (threadId: string, before: number) => {
    revoking.add(threadId);
    void revokeTabSessions(threadId, before)
      .catch((error: unknown) =>
        log(`could not revoke thread ${threadId}'s sessions: ${describeError(error)}`),
      )
      .finally(() => revoking.delete(threadId));
  };

  const mintTabSession = (threadId: string): Promise<IssuedSession> =>
    issueLocalSession({
      baseDir: paths.home,
      scopes: [AuthOrchestrationReadScope, AuthOrchestrationOperateScope],
      ttl: SESSION_TTL,
      label: "tr3x tab",
      subject: tabSubject(threadId),
      env,
    });

  /**
   * Mints the tab's session while its view starts, then writes the launch file
   * the view waits for. A tab closed meanwhile gets no file, and its session is revoked.
   */
  const handOff = async (tab: Tab, launchPath: string, newThreadProjectId: string | undefined) => {
    let session: IssuedSession;
    try {
      session = await mintTabSession(tab.threadId);
    } catch (error) {
      log(`could not start a session for thread ${tab.threadId}: ${describeError(error)}`);
      await serial(async () => {
        if (!isLive(tab)) return;
        forget(tab);
        await rex?.closeWindow(tab.sessionId, tab.windowId).catch(() => {});
      });
      return;
    }
    if (!isLive(tab)) {
      await revokeLocalSession(paths.home, session.sessionId, env).catch(() => {});
      return;
    }
    writeLaunchFile(launchPath, {
      version: 1,
      server: options.server,
      threadId: tab.threadId,
      ...(newThreadProjectId === undefined ? {} : { newThreadProjectId }),
      bearer: { token: session.token, expiresAt: session.expiresAt },
      controlSocket: paths.controlSocketPath,
    });
    launchFiles.add(launchPath);
    log(`handed thread ${tab.threadId}'s tab its session`);
    setTimeout(() => {
      launchFiles.delete(launchPath);
      NodeFS.rmSync(launchPath, { force: true });
    }, LAUNCH_FILE_TTL_MS).unref();
  };

  /**
   * The project's session: the one tr3x used for it, else one labelled with its
   * title that no other project uses, since two projects can share a title.
   */
  const sessionForProject = async (projectId: string, label: string) => {
    const known = projectSessions.get(projectId);
    if (known !== undefined) return known;
    // A session holding tabs belongs to some project, even one tr3x can't name yet,
    // such as a new thread's tab found again after a restart.
    const taken = new Set([
      ...projectSessions.values(),
      ...[...tabs.values()].map((tab) => tab.sessionId),
    ]);
    const sessions = await rex!.listSessions();
    return sessions.find((session) => session.label === label && !taken.has(session.sessionId))
      ?.sessionId;
  };

  const openTab = async (input: {
    readonly threadId: string;
    readonly title: string;
    readonly projectId: string;
    readonly projectTitle: string;
    readonly cwd: string;
    readonly focus: boolean;
    readonly newThread: boolean;
  }): Promise<OpenedTab> => {
    if (rex === undefined) throw new Error("tr3x can't find the Rex CLI. Set TR3X_REX_BIN.");
    const existing = tabs.get(input.threadId);
    if (existing !== undefined) {
      if (input.focus) await rex.focusWindow(existing.sessionId, existing.windowId);
      return existing;
    }
    hidden.delete(input.threadId);
    const launchPath = newLaunchFilePath(paths.launchDir);
    // Rex passes no environment to a block, and argv shows in `ps`: only the file's path goes here.
    const command = {
      argv: ["env", `TR3X_LAUNCH=${launchPath}`, process.execPath, CLI, "view"],
      cwd: input.cwd,
    };
    const label = rexLabel(input.title);
    const sessionLabel = rexLabel(input.projectTitle, "tr3x");
    const window = {
      windowLabel: label,
      blockLabel: tabBlockLabel(input.threadId, input.projectId),
      command,
    };
    const sessionId = await sessionForProject(input.projectId, sessionLabel);
    const placement =
      sessionId === undefined
        ? await rex.createSession({ label: sessionLabel, ...window })
        : await rex
            .createWindow({ sessionId, focus: input.focus, ...window })
            .catch(async (error: unknown) => {
              // The session went away since tr3x last saw it.
              log(`could not add a tab to session ${sessionId}: ${describeError(error)}`);
              projectSessions.delete(input.projectId);
              return await rex.createSession({ label: sessionLabel, ...window });
            });
    projectSessions.set(input.projectId, placement.sessionId);
    const tab: Tab = {
      threadId: input.threadId,
      ...placement,
      label,
      lastSeen: undefined,
      closed: false,
    };
    tabs.set(tab.threadId, tab);
    threadOfBlock.set(tab.blockId, tab.threadId);
    log(`opened thread ${tab.threadId} in session ${tab.sessionId}`);
    void handOff(tab, launchPath, input.newThread ? input.projectId : undefined);
    return tab;
  };

  const openThread = (
    projects: ReadonlyArray<ProjectShell>,
    thread: ThreadShell,
    focus: boolean,
  ) => {
    const project = projects.find((candidate) => candidate.id === thread.projectId);
    return openTab({
      threadId: thread.id,
      title: thread.title,
      projectId: thread.projectId,
      projectTitle: project?.title ?? "tr3x",
      cwd: project?.workspaceRoot ?? paths.home,
      focus,
      newThread: false,
    });
  };

  const closeTab = async (tab: Tab, why: string) => {
    forget(tab);
    log(`closing thread ${tab.threadId}'s tab: ${why}`);
    await rex?.closeWindow(tab.sessionId, tab.windowId).catch((error: unknown) => {
      log(`could not close thread ${tab.threadId}'s tab: ${describeError(error)}`);
    });
    revokeLater(tab.threadId, Date.now());
  };

  const reconcile = async () => {
    if (!mapped) return;
    const state = shellState();
    const snapshot = Option.getOrNull(state.snapshot);
    if (snapshot === null) return;
    const plan = planShell(tabs, hidden, {
      threads: snapshot.threads,
      removed: new Set(state.removedAt.keys()),
      olderSettledBefore:
        snapshot.olderSettled === undefined ? undefined : Date.parse(snapshot.olderSettled.before),
    });
    for (const threadId of plan.close) {
      const tab = tabs.get(threadId);
      if (tab !== undefined) await closeTab(tab, "the thread was archived or deleted");
    }
    for (const { threadId, label } of plan.relabel) {
      const tab = tabs.get(threadId);
      if (tab === undefined) continue;
      await rex?.setWindowLabel(tab.sessionId, tab.windowId, label).then(
        () => {
          tab.label = label;
          log(`relabelled thread ${threadId}'s tab`);
        },
        (error: unknown) =>
          log(`could not relabel thread ${threadId}'s tab: ${describeError(error)}`),
      );
    }
    for (const threadId of plan.openBlocked) {
      const thread = snapshot.threads.find((candidate) => candidate.id === threadId);
      if (thread !== undefined) {
        await openThread(snapshot.projects, thread, false).catch((error: unknown) =>
          log(`could not open blocked thread ${threadId}: ${describeError(error)}`),
        );
      }
    }
    for (const thread of snapshot.threads) {
      const tab = tabs.get(thread.id);
      if (tab !== undefined) tab.lastSeen = thread;
    }
  };
  let reconcileQueued = false;
  const scheduleReconcile = () => {
    if (reconcileQueued) return;
    reconcileQueued = true;
    void serial(async () => {
      reconcileQueued = false;
      await reconcile();
    }).catch((error: unknown) => log(`could not update tabs: ${describeError(error)}`));
  };

  /**
   * The ids a label's digest can stand for: the shell's threads and projects,
   * plus archived and older settled ones when some label holds a digest. New
   * threads' ids are short UUIDs, never digests.
   */
  const labelCandidates = async (threadDigests: ReadonlyArray<string>, needProjects: boolean) => {
    const snapshot = requireShell();
    const needThreads = threadDigests.length > 0;
    const archive =
      needThreads || needProjects
        ? await connection.archivedShell().catch((error: unknown) => {
            log(`could not read the archive: ${describeError(error)}`);
            return { projects: [], threads: [] };
          })
        : { projects: [], threads: [] };
    const ids = (items: ReadonlyArray<{ readonly id: string }>) => items.map((item) => item.id);
    const known = [...ids(snapshot.threads), ...ids(archive.threads)];
    // Page on until every digest resolves: an unresolved tab would be dropped and its session revoked.
    const unresolved = (found: ReadonlyArray<{ readonly id: string }>) => {
      const digests = new Set([...known, ...ids(found)].map(labelDigest));
      return threadDigests.some((digest) => !digests.has(digest));
    };
    const older = unresolved([])
      ? await olderSettledThreads(snapshot, (found) => !unresolved(found))
      : [];
    return {
      threadIds: [...known, ...ids(older)],
      projectIds: [...ids(snapshot.projects), ...ids(archive.projects)],
    };
  };

  /** Rebuilds the map from the blocks Rex has, so a restarted daemon finds its tabs again. */
  const rebuild = async () => {
    const sessions = await rex!.listSessions();
    const labelled: Array<
      { readonly block: RexBlock } & NonNullable<ReturnType<typeof parseTabBlockLabel>>
    > = [];
    for (const session of sessions) {
      for (const block of await rex!.listBlocks(session.sessionId)) {
        const parsed = parseTabBlockLabel(block.label);
        if (parsed !== undefined) labelled.push({ block, ...parsed });
      }
    }
    const { threadIds, projectIds } = await labelCandidates(
      labelled.flatMap((entry) => (entry.thread.kind === "digest" ? [entry.thread.digest] : [])),
      labelled.some((entry) => entry.project?.kind === "digest"),
    );
    const found = new Map<string, Tab>();
    const labelledProjects = new Map<string, string>();
    for (const { block, thread, project } of labelled) {
      const threadId = resolveLabelId(thread, threadIds);
      if (threadId === undefined) {
        log(`can't tell which thread block ${block.blockId} shows; leaving it alone`);
        continue;
      }
      if (found.has(threadId)) continue;
      const projectId = project === undefined ? undefined : resolveLabelId(project, projectIds);
      if (projectId !== undefined) labelledProjects.set(threadId, projectId);
      // Keep a known tab's own object: a hand-off in flight checks it's still the same tab.
      const previous = tabs.get(threadId);
      found.set(
        threadId,
        previous?.blockId === block.blockId
          ? previous
          : {
              threadId,
              sessionId: block.sessionId,
              windowId: block.windowId,
              blockId: block.blockId,
              label: "",
              lastSeen: undefined,
              closed: false,
            },
      );
    }
    const firstMap = !mapped;
    tabs.clear();
    threadOfBlock.clear();
    for (const tab of found.values()) {
      tabs.set(tab.threadId, tab);
      threadOfBlock.set(tab.blockId, tab.threadId);
    }
    const live = new Set(sessions.map((session) => session.sessionId));
    for (const [projectId, sessionId] of projectSessions) {
      if (!live.has(sessionId)) projectSessions.delete(projectId);
    }
    const threads = Option.getOrNull(shellState().snapshot)?.threads ?? [];
    const projectOf = new Map([
      ...threads.map((thread) => [thread.id as string, thread.projectId as string] as const),
      ...labelledProjects,
    ]);
    for (const tab of tabs.values()) {
      const projectId = projectOf.get(tab.threadId);
      if (projectId !== undefined && !projectSessions.has(projectId)) {
        projectSessions.set(projectId, tab.sessionId);
      }
    }
    mapped = true;
    log(`found ${tabs.size} thread tab(s) in Rex`);
    await closeArchivedWhileAway(threads);
    if (firstMap) void revokeLeftovers(Date.now());
    await reconcile();
  };

  /**
   * Tabs whose thread the shell doesn't list may have been archived while no
   * daemon watched; the shell has no record of that, so ask the archive.
   */
  const closeArchivedWhileAway = async (threads: ReadonlyArray<ThreadShell>) => {
    const listed = new Set(threads.map((thread) => thread.id as string));
    const unknown = [...tabs.values()].filter((tab) => !listed.has(tab.threadId));
    if (unknown.length === 0) return;
    const archived = await connection.archivedShell().then(
      (shell) => new Set(shell.threads.map((thread) => thread.id as string)),
      (error: unknown) => {
        log(`could not read the archive: ${describeError(error)}`);
        return new Set<string>();
      },
    );
    for (const tab of unknown) {
      if (archived.has(tab.threadId)) await closeTab(tab, "the thread was archived");
    }
  };

  /** A thread wherever the server keeps it: the shell, the archive, or its family for one the shell left out. */
  const findThread = async (threadId: string) => {
    const snapshot = requireShell();
    const active = snapshot.threads.find((thread) => thread.id === threadId);
    if (active !== undefined)
      return { thread: active, projects: snapshot.projects, archived: false };
    const archive = await connection.archivedShell();
    const archived = archive.threads.find((thread) => thread.id === threadId);
    if (archived !== undefined) {
      return {
        thread: archived,
        projects: [...snapshot.projects, ...archive.projects],
        archived: true,
      };
    }
    const family = await connection.threadFamily(ThreadId.make(threadId)).catch(() => undefined);
    const member = family?.threads.find((thread) => thread.id === threadId);
    return member === undefined
      ? undefined
      : { thread: member, projects: snapshot.projects, archived: false };
  };

  /** The older settled threads the shell left out, a page at a time, until `enough`. */
  const olderSettledThreads = async (
    snapshot: OrchestrationV2ShellSnapshot,
    enough: (found: ReadonlyArray<ThreadShell>) => boolean,
  ): Promise<ReadonlyArray<ThreadShell>> => {
    if (snapshot.olderSettled === undefined) return [];
    try {
      return await collectOlderSettled(
        (before) =>
          connection
            .olderSettledThreads({
              before: {
                settledAt: before.settledAt,
                ...(before.threadId === undefined
                  ? {}
                  : { threadId: ThreadId.make(before.threadId) }),
              },
              limit: OLDER_SETTLED_PAGE,
            })
            .then((page) => page.threads),
        snapshot.olderSettled.before,
        enough,
      );
    } catch (error) {
      log(`could not read older threads: ${describeError(error)}`);
      return [];
    }
  };

  /** Resolves once the shell lists the thread as active, e.g. after unarchiving it. */
  const untilActive = (threadId: string) =>
    new Promise<void>((resolve, reject) => {
      const listed = () =>
        Option.getOrNull(shellState().snapshot)?.threads.some(
          (thread) => thread.id === threadId,
        ) === true;
      if (listed()) return resolve();
      const timer = setTimeout(() => {
        stop();
        reject(new Error("The unarchived thread didn't come back; try again."));
      }, 15_000);
      const unsubscribe = connection.registry.subscribe(connection.shellStateAtom, () => {
        if (!listed()) return;
        stop();
        resolve();
      });
      const stop = () => {
        clearTimeout(timer);
        unsubscribe();
      };
    });

  /**
   * Sessions earlier daemons left: their own, and those of tabs closed while
   * no daemon ran to see it.
   */
  const revokeLeftovers = async (now: number) => {
    try {
      const listed = await listLocalSessions(paths.home, env);
      const stale = listed.filter(
        (session) =>
          (session.subject === DAEMON_SUBJECT && Date.parse(session.issuedAt) < startedAt) ||
          (session.subject.startsWith(TAB_SUBJECT_PREFIX) &&
            !tabs.has(session.subject.slice(TAB_SUBJECT_PREFIX.length)) &&
            !revoking.has(session.subject.slice(TAB_SUBJECT_PREFIX.length)) &&
            Date.parse(session.issuedAt) <= now),
      );
      for (const session of stale) await revokeLocalSession(paths.home, session.sessionId, env);
      if (stale.length > 0) log(`revoked ${stale.length} leftover session(s)`);
    } catch (error) {
      log(`could not check for leftover sessions: ${describeError(error)}`);
    }
  };

  const onRexEvent = (event: RexEvent) => {
    const at = Date.now();
    if (event.kind === "session_destroyed") {
      void serial(async () => {
        for (const [projectId, sessionId] of projectSessions) {
          if (sessionId === event.sessionId) projectSessions.delete(projectId);
        }
      });
      return;
    }
    const threadId = threadOfBlock.get(event.blockId);
    const tab = threadId === undefined ? undefined : tabs.get(threadId);
    if (tab === undefined || tab.closed) return;
    // At once, so a bearer minted from here on is revoked rather than handed out.
    tab.closed = true;
    void serial(async () => {
      forget(tab);
      const thread = Option.getOrNull(shellState().snapshot)?.threads.find(
        (candidate) => candidate.id === tab.threadId,
      );
      hidden.set(tab.threadId, thread === undefined ? null : (blockingRequestId(thread) ?? null));
      log(`thread ${tab.threadId}'s tab was closed; hiding the thread`);
      revokeLater(tab.threadId, at);
    });
  };

  // Rex restarts lose every tab, and the events stream with them: map again once it's back.
  const followRex = async () => {
    if (rex === undefined) return;
    let reported = false;
    while (!signal.aborted) {
      const stream = rex.streamEvents((line) => {
        const event = parseRexEvent(line);
        if (event !== undefined) onRexEvent(event);
      }, signal);
      await stream.started;
      try {
        await serial(rebuild);
        reported = false;
      } catch (error) {
        if (!reported) log(`can't reach Rex: ${describeError(error)}; retrying`);
        reported = true;
      }
      await stream.ended;
      if (signal.aborted) break;
      await new Promise((resolve) => setTimeout(resolve, REX_RETRY_MS));
    }
  };

  NodeFS.rmSync(paths.launchDir, { recursive: true, force: true });
  const unmount = connection.registry.mount(connection.shellStateAtom);
  const unsubscribe = connection.registry.subscribe(connection.shellStateAtom, scheduleReconcile);
  // The map needs the shell's projects; Rex comes after it.
  const following = connection.shellSnapshot(signal).then(followRex, (error: unknown) => {
    if (!signal.aborted) log(`could not load threads: ${describeError(error)}`);
  });

  const handle = async (request: ControlRequest): Promise<unknown> => {
    switch (request.op) {
      case "new": {
        const projectId = await resolveProject(connection, request.cwd, signal);
        const root = NodeFS.realpathSync(request.cwd);
        const project = requireShell().projects.find((candidate) => candidate.id === projectId);
        return await serial(() =>
          openTab({
            threadId: NodeCrypto.randomUUID(),
            title: "New thread",
            projectId,
            projectTitle: project?.title ?? NodePath.basename(root),
            cwd: project?.workspaceRoot ?? root,
            focus: true,
            newThread: true,
          }),
        ).then(({ threadId, sessionId, windowId }) => ({ threadId, sessionId, windowId }));
      }
      case "open": {
        const found = await findThread(request.threadId);
        if (found === undefined || found.thread.deletedAt !== null) {
          throw new Error(`No thread ${request.threadId}.`);
        }
        if (found.archived) {
          // Archiving closed the tab; opening it again is the way back.
          const result = await connection.unarchiveThread({ threadId: found.thread.id });
          if (!result.ok) throw new Error(`Could not unarchive the thread: ${result.error}`);
          await untilActive(found.thread.id);
        }
        return await serial(() => openThread(found.projects, found.thread, true)).then(
          ({ threadId, sessionId, windowId }) => ({ threadId, sessionId, windowId }),
        );
      }
      case "next-blocked": {
        const thread = nextBlockedThread(requireShell().threads, lastNextBlocked);
        if (thread === undefined) return null;
        lastNextBlocked = thread.id;
        return await serial(() => openThread(requireShell().projects, thread, true)).then(
          ({ threadId, sessionId, windowId }) => ({ threadId, sessionId, windowId }),
        );
      }
      case "threads": {
        const snapshot = requireShell();
        const archive = await connection.archivedShell().catch((error: unknown) => {
          log(`could not read the archive: ${describeError(error)}`);
          return { projects: [], threads: [] };
        });
        const projects = new Map(
          [...archive.projects, ...snapshot.projects].map((project) => [project.id, project.title]),
        );
        const entry =
          (archived: boolean) =>
          (thread: ThreadShell): ThreadEntry => ({
            threadId: thread.id,
            title: thread.title,
            project: projects.get(thread.projectId) ?? "",
            open: tabs.has(thread.id),
            blocked: !archived && blockingRequestId(thread) !== undefined,
            archived,
          });
        const topLevel = (thread: ThreadShell) =>
          thread.lineage.relationshipToParent !== "subagent" && thread.deletedAt === null;
        const listed = new Set(snapshot.threads.map((thread) => thread.id as string));
        // The picker lists up to 1,000 of them; `tr3x open` reaches any thread by id.
        const older = (
          await olderSettledThreads(snapshot, (found) => found.length >= 1_000)
        ).filter((thread) => !listed.has(thread.id));
        return [
          ...[...snapshot.threads, ...older].filter(topLevel).map(entry(false)),
          ...archive.threads.filter(topLevel).map(entry(true)),
        ];
      }
      case "bearer": {
        // Only a live tab gets one, and a tab that closes while it's minted gets none.
        const tab = tabs.get(request.threadId);
        if (tab === undefined || tab.closed) throw new Error("That thread has no tab.");
        const session = await mintTabSession(tab.threadId);
        if (!isLive(tab)) {
          await revokeLocalSession(paths.home, session.sessionId, env).catch(() => {});
          throw new Error("That thread's tab closed.");
        }
        return session;
      }
    }
  };

  return {
    handle,
    stop: async () => {
      stopController.abort();
      await following;
      await queue;
      unsubscribe();
      unmount();
      connection.dispose();
      for (const path of launchFiles) NodeFS.rmSync(path, { force: true });
    },
  };
}
