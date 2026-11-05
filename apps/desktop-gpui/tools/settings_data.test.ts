// Writes src/settings/contract.json from the TypeScript sources it mirrors.
// Regenerate: copy this file to apps/web/src/ and run `vp test run src/settings_data.test.ts` from apps/web.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import * as Schema from "effect/Schema";
import { it } from "vitest";
import {
  ClientSettingsSchema,
  DEFAULT_CLIENT_SETTINGS,
  DEFAULT_SERVER_SETTINGS,
  isNullableProjectSettingsOverride,
  PROJECT_FILE_BACKED_SETTINGS,
  PROJECT_SCOPED_SERVER_SETTING_KEYS,
  ServerSettings,
} from "@t3tools/contracts";
import { SETTINGS_SEARCH_ITEMS } from "~/components/settings/settingsSearch";

it("writes the desktop-gpui settings contract", () => {
  const data = {
    serverDefaults: Schema.encodeSync(ServerSettings)(DEFAULT_SERVER_SETTINGS),
    clientDefaults: Schema.encodeSync(ClientSettingsSchema)(DEFAULT_CLIENT_SETTINGS),
    projectScopedKeys: PROJECT_SCOPED_SERVER_SETTING_KEYS,
    nullableOverrideKeys: PROJECT_SCOPED_SERVER_SETTING_KEYS.filter(isNullableProjectSettingsOverride),
    fileBackedBuiltIns: Object.fromEntries(
      Object.entries(PROJECT_FILE_BACKED_SETTINGS).map(([key, { builtIn }]) => [key, builtIn]),
    ),
    search: SETTINGS_SEARCH_ITEMS.map((item) => ({ ...item, to: item.to.replace("/settings/", "") })),
  };
  const out = process.env.SETTINGS_CONTRACT_OUT ?? fileURLToPath(new URL("./contract.json", import.meta.url));
  writeFileSync(out, `${JSON.stringify(data, null, 1)}\n`);
});
