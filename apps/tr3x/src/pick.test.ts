import { describe, expect, it } from "vite-plus/test";

import { pickLines } from "./pick.ts";

describe("pickLines", () => {
  it("keeps terminal control sequences in titles off the screen", () => {
    const [line] = pickLines(
      [
        {
          threadId: "t1",
          title: "\u001b[2J\u001b[HFake\u0007 title",
          project: "work\u001b]0;x\u0007",
          open: false,
          blocked: false,
          archived: false,
        },
      ],
      0,
      10,
    );
    expect(line).not.toMatch(/[\u0000-\u001f\u007f-\u009f]/u);
    expect(line).toContain("Fake  title");
  });
});
