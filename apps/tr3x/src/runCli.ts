// @effect-diagnostics nodeBuiltinImport:off globalDate:off -- The CLI runs on Bun outside the Effect runtime.
import * as NodePath from "node:path";

import { parseArgs, USAGE } from "./args.ts";
import { checkBunRuntime } from "./bunRuntime.ts";
import { controlRequest } from "./daemon/control.ts";
import { runDaemon } from "./daemon/daemon.ts";
import { runDoctor } from "./doctor.ts";
import { resolveTr3xPaths } from "./paths.ts";
import { rexSetupInstructions } from "./rex/setup.ts";

export interface CliIo {
  readonly env: NodeJS.ProcessEnv;
  readonly cwd: string;
  /** `process.versions.bun`: undefined outside Bun. */
  readonly bunVersion: string | undefined;
  readonly out: (text: string) => void;
  readonly err: (text: string) => void;
}

/** Runs one `tr3x` command and returns its exit code. */
export async function runCli(argv: ReadonlyArray<string>, io: CliIo): Promise<number> {
  if (argv.length === 0) {
    io.out(USAGE);
    return 2;
  }
  const command = parseArgs(argv, io.cwd);
  if (command === null) {
    io.err(`tr3x: unknown command "${argv.join(" ")}"\n\n${USAGE}`);
    return 2;
  }
  if (command.kind === "help") {
    io.out(USAGE);
    return 0;
  }
  // Doctor runs anyway: it reports the Bun alongside everything else.
  const bun = checkBunRuntime(io.bunVersion);
  if (!bun.ok && command.kind !== "doctor") {
    io.err(`tr3x: ${bun.detail}\n`);
    return 1;
  }
  const paths = resolveTr3xPaths(io.env);
  if (command.kind === "setup-rex") {
    io.out(rexSetupInstructions(io.env));
    return 0;
  }
  if (command.kind === "tab-new" || command.kind === "open" || command.kind === "next-blocked") {
    // One JSON line: where the thread's tab is, for the Rex glue to show its session.
    const result = await controlRequest(
      paths.controlSocketPath,
      command.kind === "tab-new"
        ? { op: "new", cwd: NodePath.resolve(io.cwd, command.cwd) }
        : command.kind === "open"
          ? { op: "open", threadId: command.threadId }
          : { op: "next-blocked" },
    );
    io.out(`${JSON.stringify(result)}\n`);
    return 0;
  }
  if (command.kind === "pick") {
    const { runPick } = await import("./pick.ts");
    return await runPick(paths, io.env, command.clientId);
  }
  if (command.kind === "launch" && !io.env.TR3X_LAUNCH) {
    io.err(`tr3x: view needs a thread id\n\n${USAGE}`);
    return 2;
  }
  if (command.kind === "view" || command.kind === "new" || command.kind === "launch") {
    // Loaded only here, so daemon and doctor never load OpenTUI.
    const { runView } = await import("./view/main.tsx");
    const { ThreadId } = await import("@t3tools/contracts");
    return await runView({
      paths,
      env: io.env,
      target:
        command.kind === "launch"
          ? { kind: "launch", path: io.env.TR3X_LAUNCH! }
          : command.kind === "view"
            ? { kind: "thread", threadId: ThreadId.make(command.threadId) }
            : { kind: "new", cwd: command.cwd },
    });
  }
  if (command.kind === "doctor") {
    return await runDoctor({
      paths,
      env: io.env,
      bunVersion: io.bunVersion,
      out: (line) => io.out(`${line}\n`),
    });
  }
  const controller = new AbortController();
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
    process.on(signal, () => controller.abort());
  }
  return await runDaemon({
    paths,
    env: io.env,
    signal: controller.signal,
    log: (line) => io.out(`${new Date().toISOString()} tr3x daemon: ${line}\n`),
  });
}
