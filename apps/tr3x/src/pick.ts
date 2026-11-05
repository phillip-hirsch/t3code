// @effect-diagnostics nodeBuiltinImport:off -- tr3x runs on Bun outside the Effect runtime.
import * as NodeReadline from "node:readline";

import { controlRequest } from "./daemon/control.ts";
import type { ThreadEntry } from "./daemon/tabService.ts";
import { rexLabel } from "./daemon/tabs.ts";
import type { Tr3xPaths } from "./paths.ts";
import { makeRexClient } from "./rex/rex.ts";
import { describeError } from "./redact.ts";

/** Titles come from models and users: no control characters reach the terminal. */
const shown = (text: string) => rexLabel(text, "");

/** Threads whose title or project holds every word of `query`, waiting ones first. */
export function filterThreads(
  threads: ReadonlyArray<ThreadEntry>,
  query: string,
): ReadonlyArray<ThreadEntry> {
  const words = query.toLowerCase().split(/\s+/u).filter(Boolean);
  return threads
    .filter((thread) => {
      const text = `${thread.title} ${thread.project}`.toLowerCase();
      return words.every((word) => text.includes(word));
    })
    .toSorted((a, b) => Number(b.blocked) - Number(a.blocked));
}

/** One line per thread: a marker for waiting (!), open (•) or archived (a), the title, the project. */
export function pickLines(
  threads: ReadonlyArray<ThreadEntry>,
  selected: number,
  rows: number,
): ReadonlyArray<string> {
  const start = Math.max(0, Math.min(selected - rows + 1, threads.length - rows));
  return threads.slice(start, start + rows).map((thread, offset) => {
    const marker = thread.archived ? "a" : thread.blocked ? "!" : thread.open ? "•" : " ";
    const cursor = start + offset === selected ? ">" : " ";
    return `${cursor} ${marker} ${shown(thread.title)}  (${shown(thread.project)})`;
  });
}

const decodeThreads = (value: unknown): ReadonlyArray<ThreadEntry> =>
  Array.isArray(value) ? (value as ReadonlyArray<ThreadEntry>) : [];

/**
 * `tr3x pick`: a filterable list of threads, archived ones too. Enter opens one
 * as a tab (unarchiving it); Escape leaves. Without a terminal, it prints the list.
 */
export async function runPick(
  paths: Tr3xPaths,
  env: NodeJS.ProcessEnv,
  clientId: string | undefined,
): Promise<number> {
  const threads = decodeThreads(await controlRequest(paths.controlSocketPath, { op: "threads" }));
  const { stdin, stdout } = process;
  if (!stdin.isTTY) {
    for (const thread of threads) {
      const state = thread.archived
        ? "archived"
        : thread.blocked
          ? "blocked"
          : thread.open
            ? "open"
            : "";
      stdout.write(
        `${thread.threadId}\t${shown(thread.title)}\t${shown(thread.project)}\t${state}\n`,
      );
    }
    return 0;
  }
  let query = "";
  let selected = 0;
  let message = "";
  const draw = () => {
    const shown = filterThreads(threads, query);
    selected = Math.max(0, Math.min(selected, shown.length - 1));
    const rows = Math.max(1, (stdout.rows ?? 24) - 4);
    const lines = pickLines(shown, selected, rows);
    stdout.write(
      `\x1b[2J\x1b[HOpen a thread: ${query}\n${message}\n${
        lines.length === 0 ? "  No threads match." : lines.join("\n")
      }\n\x1b[1;${16 + query.length}H`,
    );
    return shown;
  };
  NodeReadline.emitKeypressEvents(stdin);
  stdin.setRawMode(true);
  return await new Promise<number>((resolve) => {
    const finish = (code: number) => {
      stdin.setRawMode(false);
      stdin.pause();
      stdout.write("\x1b[2J\x1b[H");
      resolve(code);
    };
    let shown = draw();
    let opening = false;
    stdin.on("keypress", (text: string | undefined, key: NodeReadline.Key) => {
      if (opening) return;
      if (key.name === "escape" || (key.ctrl && key.name === "c")) return finish(0);
      if (key.name === "return") {
        const thread = shown[selected];
        if (thread === undefined) return;
        opening = true;
        void controlRequest(paths.controlSocketPath, {
          op: "open",
          threadId: thread.threadId,
        })
          .then(async (opened) => {
            const sessionId = (opened as { readonly sessionId?: unknown } | null)?.sessionId;
            // The tab may be in another project's session, which the app isn't showing.
            if (
              clientId !== undefined &&
              typeof sessionId === "string" &&
              sessionId !== env.REX_SESSION
            ) {
              await makeRexClient(env)
                .selectSession(clientId, sessionId)
                .catch(() => {});
            }
          })
          .then(
            () => finish(0),
            (error: unknown) => {
              opening = false;
              message = `Could not open it: ${describeError(error)}`;
              shown = draw();
            },
          );
        return;
      }
      if (key.name === "up" || (key.ctrl && key.name === "p")) selected -= 1;
      else if (key.name === "down" || (key.ctrl && key.name === "n")) selected += 1;
      else if (key.name === "backspace") query = query.slice(0, -1);
      else if (text !== undefined && !key.ctrl && !key.meta && text >= " ") query += text;
      shown = draw();
    });
  });
}
