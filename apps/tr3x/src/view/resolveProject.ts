// @effect-diagnostics nodeBuiltinImport:off -- tr3x runs on Bun outside the Effect runtime.
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import { ProjectId } from "@t3tools/contracts";

import type { ViewConnection } from "./connection.ts";

/** The project rooted at `cwd`, created when the server has none. */
export async function resolveProject(
  connection: Pick<ViewConnection, "shellSnapshot" | "createProject">,
  cwd: string,
  signal: AbortSignal,
): Promise<ProjectId> {
  const root = NodeFS.realpathSync(cwd);
  // The server resolves a workspace root without following symlinks, so a
  // project made elsewhere under a symlinked path is found by that path too.
  const roots = new Set([NodePath.resolve(cwd), root]);
  const snapshot = await connection.shellSnapshot(signal);
  const existing = snapshot.projects.find((project) => roots.has(project.workspaceRoot));
  if (existing) return existing.id;
  const projectId = ProjectId.make(NodeCrypto.randomUUID());
  const result = await connection.createProject({
    projectId,
    title: NodePath.basename(root) || root,
    workspaceRoot: root,
  });
  if (!result.ok) throw new Error(`Could not create a project for ${root}: ${result.error}`);
  return projectId;
}
