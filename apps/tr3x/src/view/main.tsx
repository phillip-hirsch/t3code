// @effect-diagnostics nodeBuiltinImport:off globalDate:off -- The view runs on Bun outside the Effect runtime.
import * as NodeCrypto from "node:crypto";
import { createCliRenderer, type CliRenderer } from "@opentui/core";
import { createRoot } from "@opentui/react";
import { RegistryContext } from "@effect/atom-react";
import {
  AuthOrchestrationOperateScope,
  AuthOrchestrationReadScope,
  EnvironmentId,
  ProjectId,
  ThreadId,
} from "@t3tools/contracts";
import * as Schema from "effect/Schema";

import { controlRequest } from "../daemon/control.ts";
import { readLiveDaemonState } from "../daemon/daemonState.ts";
import { fetchRecordedEnvironment } from "../daemon/health.ts";
import { waitForLaunchFile } from "../daemon/launchFile.ts";
import { IssuedSession, issueLocalSession } from "../daemon/localSession.ts";
import type { Tr3xPaths } from "../paths.ts";
import { describeError } from "../redact.ts";
import { App } from "./App.tsx";
import { resolveProject } from "./resolveProject.ts";
import { createViewConnection, type ViewConnection } from "./connection.ts";
import type { ViewServer } from "./platform.ts";
import { sessionBearer } from "./sessionBearer.ts";
import { Startup } from "./Startup.tsx";
import type { RexStatus } from "./status.ts";

export type ViewTarget =
  | { readonly kind: "thread"; readonly threadId: ThreadId }
  | { readonly kind: "new"; readonly cwd: string }
  /** A daemon's tab: the launch file names the server, the thread, and a bearer. */
  | { readonly kind: "launch"; readonly path: string };

/** What a mounted view shows: a thread, or a new one in a project, launched by its first message. */
export type MountTarget =
  | { readonly kind: "thread"; readonly threadId: ThreadId }
  | { readonly kind: "new"; readonly cwd: string }
  | { readonly kind: "new-in-project"; readonly threadId: ThreadId; readonly projectId: ProjectId };

const SESSION_TTL = "1h";
/** Re-mint this long before expiry, so a reconnect never presents a dying bearer. */
const SESSION_REFRESH_MS = 5 * 60_000;

/**
 * The daemon's server, with a bearer minted on demand. The bearer is scoped to
 * reading and operating threads and lives an hour; client-runtime asks for it
 * again on every reconnect, so a fresh one is minted once it nears expiry.
 */
export async function resolveViewServer(
  paths: Tr3xPaths,
  env: NodeJS.ProcessEnv,
): Promise<ViewServer> {
  const daemon = readLiveDaemonState(paths.daemonStatePath);
  if (daemon === undefined) {
    throw new Error(`No tr3x daemon is running for ${paths.home}. Start it with \`tr3x daemon\`.`);
  }
  // Never send a credential to a server the daemon didn't record.
  const environment = await fetchRecordedEnvironment(daemon);
  return {
    environmentId: environment.environmentId,
    label: environment.label,
    httpBaseUrl: daemon.httpBaseUrl,
    wsBaseUrl: daemon.wsBaseUrl,
    bearerToken: sessionBearer(
      () =>
        issueLocalSession({
          baseDir: paths.home,
          scopes: [AuthOrchestrationReadScope, AuthOrchestrationOperateScope],
          ttl: SESSION_TTL,
          label: "tr3x view",
          env,
        }),
      SESSION_REFRESH_MS,
    ),
  };
}

const decodeIssuedSession = Schema.decodeUnknownSync(IssuedSession);

/**
 * The server and thread a daemon's launch file names. The file's bearer serves
 * first; the daemon mints each later one, so it can revoke them all when the tab closes.
 */
export async function resolveLaunch(
  path: string,
  signal: AbortSignal,
): Promise<{ readonly server: ViewServer; readonly target: MountTarget }> {
  const launch = await waitForLaunchFile(path, signal);
  const threadId = ThreadId.make(launch.threadId);
  let first: IssuedSession | null = { sessionId: "", ...launch.bearer };
  const server: ViewServer = {
    ...launch.server,
    environmentId: EnvironmentId.make(launch.server.environmentId),
    bearerToken: sessionBearer(async () => {
      const issued = first;
      first = null;
      if (issued !== null) return issued;
      return decodeIssuedSession(
        await controlRequest(launch.controlSocket, { op: "bearer", threadId }),
      );
    }, SESSION_REFRESH_MS),
  };
  return {
    server,
    target:
      launch.newThreadProjectId === undefined
        ? { kind: "thread", threadId }
        : {
            kind: "new-in-project",
            threadId,
            projectId: ProjectId.make(launch.newThreadProjectId),
          },
  };
}

/** `promise`, or a rejection as soon as `signal` aborts. */
function untilAborted<A>(promise: Promise<A>, signal: AbortSignal): Promise<A> {
  return new Promise((resolve, reject) => {
    const onAbort = () => reject(new Error("Stopped."));
    if (signal.aborted) return onAbort();
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", onAbort));
  });
}

export interface MountedView {
  readonly connection: ViewConnection;
  readonly threadId: ThreadId;
  readonly dispose: () => void;
}

/** How startup ended: the view is up, the user quit, or it can't start. */
export type ViewStart =
  | { readonly kind: "mounted"; readonly view: MountedView }
  | { readonly kind: "quit" }
  | { readonly kind: "failed"; readonly message: string };

/**
 * Renders the view into `renderer`; the CLI and the scripted checks share this.
 * A startup screen shows at once, and ctrl+c (`onQuit`) or `signal` ends it. It
 * signs in before anything waits on the server, so a session that can't be
 * minted fails startup instead of leaving it waiting.
 */
export async function mountView(input: {
  readonly renderer: CliRenderer;
  readonly server: ViewServer;
  readonly target: MountTarget;
  readonly signal: AbortSignal;
  readonly onStatus?: (status: RexStatus) => void;
  readonly onQuit: () => void;
  /** Opens the connection; tests pass one that needs no server. */
  readonly connect?: (server: ViewServer) => ViewConnection;
}): Promise<ViewStart> {
  const { renderer, target, signal } = input;
  const root = createRoot(renderer);
  const startup = (message: string, connection: ViewConnection | null) =>
    root.render(
      connection === null ? (
        <Startup message={message} connection={null} onQuit={input.onQuit} />
      ) : (
        <RegistryContext.Provider value={connection.registry}>
          <Startup message={message} connection={connection} onQuit={input.onQuit} />
        </RegistryContext.Provider>
      ),
    );
  const stop = (connection: ViewConnection | null, outcome: ViewStart): ViewStart => {
    root.unmount();
    connection?.dispose();
    return outcome;
  };

  startup("Connecting to the tr3x server…", null);
  try {
    await untilAborted(input.server.bearerToken(), signal);
  } catch (error) {
    if (signal.aborted) return stop(null, { kind: "quit" });
    return stop(null, {
      kind: "failed",
      message: `Could not sign in to the tr3x server: ${describeError(error)}`,
    });
  }
  const connection = (input.connect ?? createViewConnection)(input.server);
  let newThread: { readonly projectId: ProjectId } | null =
    target.kind === "new-in-project" ? { projectId: target.projectId } : null;
  if (target.kind === "new") {
    startup("Loading projects…", connection);
    try {
      // Each step can stall on the server (the shell, then creating the project); quit wins.
      newThread = {
        projectId: await untilAborted(resolveProject(connection, target.cwd, signal), signal),
      };
    } catch (error) {
      if (signal.aborted) return stop(connection, { kind: "quit" });
      return stop(connection, { kind: "failed", message: describeError(error) });
    }
  }
  if (signal.aborted) return stop(connection, { kind: "quit" });
  const threadId = target.kind === "new" ? ThreadId.make(NodeCrypto.randomUUID()) : target.threadId;
  const onStatus = (status: RexStatus) => {
    try {
      renderer.setProgramStatus(status);
      renderer.setTerminalTitle(status.title);
    } catch {
      // A report the terminal can't take must not take the view down.
    }
    input.onStatus?.(status);
  };
  root.render(
    <RegistryContext.Provider value={connection.registry}>
      <App
        connection={connection}
        threadId={threadId}
        newThread={newThread}
        onStatus={onStatus}
        onQuit={input.onQuit}
      />
    </RegistryContext.Provider>,
  );
  return {
    kind: "mounted",
    view: { connection, threadId, dispose: () => stop(connection, { kind: "quit" }) },
  };
}

/** `tr3x view`: runs until the user quits; 1 when it can't start. */
export async function runView(input: {
  readonly paths: Tr3xPaths;
  readonly env: NodeJS.ProcessEnv;
  readonly target: ViewTarget;
}): Promise<number> {
  // Before anything waits: a signal ends the view at any point.
  const quit = new AbortController();
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
    process.once(signal, () => quit.abort());
  }
  let server: ViewServer;
  let target: MountTarget;
  if (input.target.kind === "launch") {
    process.stdout.write("Connecting to the tr3x server…\n");
    ({ server, target } = await resolveLaunch(input.target.path, quit.signal));
  } else {
    server = await resolveViewServer(input.paths, input.env);
    target = input.target;
  }
  if (quit.signal.aborted) return 0;
  const renderer = await createCliRenderer({
    exitOnCtrlC: false,
    useMouse: true,
    autoFocus: false,
  });
  const start = await mountView({
    renderer,
    server,
    target,
    signal: quit.signal,
    onQuit: () => quit.abort(),
  });
  if (start.kind === "mounted") {
    if (!quit.signal.aborted) {
      await new Promise((resolve) =>
        quit.signal.addEventListener("abort", resolve, { once: true }),
      );
    }
    // Records belong to the terminal; leave none behind for a view that's gone.
    renderer.setProgramStatus({ state: "clear" });
    start.view.dispose();
  }
  renderer.destroy();
  await renderer.closed;
  if (start.kind !== "failed") return 0;
  process.stderr.write(`tr3x: ${start.message}\n`);
  return 1;
}
