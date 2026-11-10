// Builds assets/vendor/office-engine.zip from the web's Office workers, their @betteroffice engines, wasm and fonts
// (with their licenses), and the helpers in office-engine/entry.ts. Rerun when the web's Office preview changes:
//   node apps/desktop-gpui/tools/office_engine.mjs
import * as NodeFS from "node:fs";
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import { build } from "vite-plus";

const here = NodePath.dirname(NodeURL.fileURLToPath(import.meta.url));
const web = NodePath.resolve(here, "../../web");
const out = NodePath.resolve(here, "../target/office-engine");
const archive = NodePath.resolve(here, "../assets/vendor/office-engine.zip");
const requireFromWeb = NodeModule.createRequire(NodePath.join(web, "package.json"));
const JSZip = requireFromWeb("jszip");
const xlsx = NodeFS.realpathSync(NodePath.join(web, "node_modules/@betteroffice/xlsx"));

await build({
  configFile: false,
  root: here,
  logLevel: "warn",
  resolve: {
    tsconfigPaths: true,
    alias: {
      // The web aliases the 33 MB CJK font add-on away (see fonts-cjk.ts), and so does this bundle.
      "@betteroffice/fonts-cjk": NodePath.join(web, "src/components/files/office/fonts-cjk.ts"),
      "@betteroffice/xlsx/headless": NodePath.join(xlsx, "dist/headless.js"),
    },
  },
  assetsInclude: ["**/*.wasm"],
  worker: { format: "es" },
  build: {
    outDir: out,
    emptyOutDir: true,
    target: "es2022",
    assetsInlineLimit: 0,
    modulePreload: false,
    copyPublicDir: false,
    reportCompressedSize: false,
    rollupOptions: {
      input: { engine: NodePath.join(here, "office-engine/entry.ts") },
      preserveEntrySignatures: "strict",
      output: {
        entryFileNames: "[name].js",
        chunkFileNames: "[name]-[hash].js",
        assetFileNames: "[name]-[hash][extname]",
      },
    },
  },
});

const zip = new JSZip();
// A fixed date keeps the archive byte-identical across rebuilds of the same sources.
const date = new Date("2026-01-01T00:00:00Z");
const files = NodeFS.readdirSync(out, { recursive: true, withFileTypes: true })
  .filter((entry) => entry.isFile())
  .map((entry) =>
    NodePath.relative(out, NodePath.join(entry.parentPath, entry.name)).replaceAll("\\", "/"),
  )
  .toSorted();
for (const file of files)
  zip.file(file, NodeFS.readFileSync(NodePath.join(out, file)), { date, createFolders: false });
const fontLicenses = NodePath.join(web, "node_modules/@betteroffice/fonts/LICENSES");
for (const [name, source] of [
  ["LICENSE-betteroffice", NodePath.join(xlsx, "LICENSE")],
  ["LICENSE-opencode", NodePath.join(web, "src/components/files/office/LICENSE.opencode")],
  ...NodeFS.readdirSync(fontLicenses)
    .toSorted()
    .map((file) => [`LICENSES-fonts/${file}`, NodePath.join(fontLicenses, file)]),
])
  zip.file(name, NodeFS.readFileSync(source), { date, createFolders: false });
const bytes = await zip.generateAsync({
  type: "nodebuffer",
  compression: "DEFLATE",
  compressionOptions: { level: 9 },
  platform: "UNIX",
});
NodeFS.writeFileSync(archive, bytes);
console.log(`${files.length} files, ${(bytes.length / 1048576).toFixed(1)} MB -> ${archive}`);
