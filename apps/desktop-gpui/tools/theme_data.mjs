// Writes src/themes.json from `BUILT_IN_THEMES` and src/theme_standard.json from the role list,
// the reserved ids and the standard palettes in packages/shared/src/themePalettes.ts.
// Regenerate with Node 23+ (it strips the TypeScript types): node apps/desktop-gpui/tools/theme_data.mjs
import { writeFileSync } from "node:fs";
import {
  BUILT_IN_THEMES,
  RESERVED_THEME_IDS,
  T3_CODE_DARK_THEME_COLORS,
  T3_CODE_LIGHT_THEME_COLORS,
  THEME_COLOR_ROLES,
} from "../../../packages/shared/src/themePalettes.ts";

const write = (file, value) =>
  writeFileSync(new URL(`../src/${file}`, import.meta.url), `${JSON.stringify(value, null, 1)}\n`);
write("themes.json", BUILT_IN_THEMES);
write("theme_standard.json", {
  roles: THEME_COLOR_ROLES,
  reserved: [...RESERVED_THEME_IDS],
  light: T3_CODE_LIGHT_THEME_COLORS,
  dark: T3_CODE_DARK_THEME_COLORS,
});
