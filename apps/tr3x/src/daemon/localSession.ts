// @effect-diagnostics nodeBuiltinImport:off -- tr3x runs on Bun outside the Effect runtime.
import * as NodeChildProcess from "node:child_process";
import type { AuthGrantScope } from "@t3tools/contracts";
import * as Schema from "effect/Schema";

import { resolveServerCommand, stripReservedLaunchEnv } from "./serverLauncher.ts";

/**
 * Mints a scoped, short-lived session through upstream's local auth control
 * plane (`t3 auth session issue`), so no long-lived bearer is stored anywhere.
 * The token comes back on the child's stdout, never in argv.
 */

export const IssuedSession = Schema.Struct({
  sessionId: Schema.String,
  token: Schema.String,
  expiresAt: Schema.String,
});
export type IssuedSession = typeof IssuedSession.Type;

const decodeIssuedSession = Schema.decodeUnknownSync(Schema.fromJsonString(IssuedSession));

export interface IssueLocalSessionInput {
  readonly baseDir: string;
  readonly scopes: ReadonlyArray<AuthGrantScope>;
  /** Upstream's duration syntax, e.g. `2m`. */
  readonly ttl: string;
  readonly label: string;
  /** Groups sessions, e.g. every one minted for a tab, so they can be found and revoked together. */
  readonly subject?: string;
  readonly env: NodeJS.ProcessEnv;
}

export async function issueLocalSession(input: IssueLocalSessionInput): Promise<IssuedSession> {
  const stdout = await runAuthSession(input.env, [
    "issue",
    "--base-dir",
    input.baseDir,
    ...input.scopes.flatMap((scope) => ["--scope", scope]),
    "--ttl",
    input.ttl,
    "--label",
    input.label,
    ...(input.subject === undefined ? [] : ["--subject", input.subject]),
    "--json",
  ]);
  return decodeIssuedSession(stdout.trim());
}

const ListedSessions = Schema.Array(
  Schema.Struct({ sessionId: Schema.String, subject: Schema.String, issuedAt: Schema.String }),
);
export type ListedSession = (typeof ListedSessions.Type)[number];
const decodeListedSessions = Schema.decodeUnknownSync(Schema.fromJsonString(ListedSessions));

/** The base dir's active sessions, without their tokens. */
export async function listLocalSessions(
  baseDir: string,
  env: NodeJS.ProcessEnv,
): Promise<ReadonlyArray<ListedSession>> {
  return decodeListedSessions(
    (await runAuthSession(env, ["list", "--base-dir", baseDir, "--json"])).trim(),
  );
}

export async function revokeLocalSession(
  baseDir: string,
  sessionId: string,
  env: NodeJS.ProcessEnv,
): Promise<void> {
  await runAuthSession(env, ["revoke", "--base-dir", baseDir, sessionId]);
}

/** Runs `t3 auth session <args>` and returns its stdout. Tokens come back there, never in argv. */
function runAuthSession(
  env: NodeJS.ProcessEnv,
  sessionArgs: ReadonlyArray<string>,
): Promise<string> {
  const { command, args: entryArgs, cwd } = resolveServerCommand(env);
  const args = [...entryArgs, "auth", "session", ...sessionArgs];
  return new Promise<string>((resolve, reject) => {
    NodeChildProcess.execFile(
      command,
      args,
      { cwd, env: stripReservedLaunchEnv(env), timeout: 20_000, encoding: "utf8" },
      (error, out, err) => {
        if (error) {
          // A Node that can't run says so in the spawn error, not on stderr.
          const detail =
            err.trim().split("\n").at(-1) ||
            (error.code === "ENOENT" || error.code === "EACCES"
              ? `${command} can't run (${error.code}). Set TR3X_NODE_BIN to a Node binary.`
              : error.killed
                ? "it took too long"
                : undefined);
          // Not "session: …": redaction reads that as a credential and hides the reason.
          reject(new Error(`Could not ${sessionArgs[0]} a session.${detail ? ` ${detail}` : ""}`));
        } else resolve(out);
      },
    );
  });
}
