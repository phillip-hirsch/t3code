// Writes src/themes.json from `BUILT_IN_THEMES` in packages/shared/src/themePalettes.ts.
// Regenerate with Node 23+ (it strips the TypeScript types): node apps/desktop-gpui/tools/theme_data.mjs
import { writeFileSync } from "node:fs";
import { BUILT_IN_THEMES } from "../../../packages/shared/src/themePalettes.ts";

writeFileSync(new URL("../src/themes.json", import.meta.url), `${JSON.stringify(BUILT_IN_THEMES, null, 1)}\n`);
