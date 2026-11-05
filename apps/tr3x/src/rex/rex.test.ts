import { describe, expect, it } from "vite-plus/test";

import { parseRexEvent } from "./rex.ts";

// Lines as `rex events --all --json` printed them (API revision 7).
const SESSION = "session:01a1840c-1d01-7bf2-8b2e-8308751b09a3";
const BLOCK_CLOSED = `{"type":"session_event","payload":{"session_id":"${SESSION}","event":{"type":"block_closed","payload":{"block_id":"block:01a1840c-1d01-7c6b-a169-b7633ef1ca20"}}}}`;
const WINDOW_CLOSED = `{"type":"session_event","payload":{"session_id":"${SESSION}","event":{"type":"window_closed","payload":{"window_id":"window:01a1840c-1d03-7217-80de-a1d7934b6bc7"}}}}`;
const DESTROYED = `{"type":"session_destroyed","payload":{"session_id":"${SESSION}"}}`;
const CLIENT_LEFT = `{"type":"client_left","payload":{"client_id":"client:01a18408-a231-7586-8b36-a3583aa4cc03"}}`;

describe("parseRexEvent", () => {
  it("reads closed blocks and destroyed sessions, and skips everything else", () => {
    expect(parseRexEvent(BLOCK_CLOSED)).toEqual({
      kind: "block_closed",
      sessionId: SESSION,
      blockId: "block:01a1840c-1d01-7c6b-a169-b7633ef1ca20",
    });
    expect(parseRexEvent(DESTROYED)).toEqual({ kind: "session_destroyed", sessionId: SESSION });
    expect(parseRexEvent(WINDOW_CLOSED)).toBeUndefined();
    expect(parseRexEvent(CLIENT_LEFT)).toBeUndefined();
    expect(parseRexEvent("not json")).toBeUndefined();
  });
});
