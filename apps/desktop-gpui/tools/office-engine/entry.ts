// The Office viewer's engine bundle (`assets/vendor/office-engine.zip`): the web's Office workers and the
// main-thread helpers its viewers use, built from apps/web by `../office_engine.mjs`. The viewer page itself
// (`assets/office-viewer`) is served as written and imports this bundle's `engine.js`.
export { createWorkerClient } from "../../../web/src/components/files/office/worker-rpc";
export {
  columnName,
  contains,
  coverMerges,
  lastCell,
  settleMerges,
  skipHidden,
} from "../../../web/src/components/files/office/sheet-cells";
export {
  headerHeight,
  paintCells,
  paintHeaders,
  paintSelection,
  shiftGrid,
} from "../../../web/src/components/files/office/sheet-paint";
export { externalUrl } from "../../../web/src/components/files/office/links";
export { layerFont, lineHeight } from "../../../web/src/components/files/office/word-protocol";
export { maxImageSide } from "../../../web/src/components/files/office/slide-protocol";
export { OFFICE_PREVIEW_MAX_BYTES } from "../../../web/src/components/files/officePreviewProtocol";
export {
  cellAtPoint,
  extendTo,
  hyperlinkAtCell,
  normalizeRange,
  parseHyperlinkLocation,
  safeExternalHyperlink,
  selectionAt,
  selectionKeyReducer,
} from "@betteroffice/xlsx/headless";

/** The workers, one per open file: the preparation step that bounds the package, then its kind's engine. */
export const workers = {
  prepare: () =>
    new Worker(
      new URL("../../../web/src/components/files/officePreview.worker.ts", import.meta.url),
      {
        type: "module",
      },
    ),
  docx: () =>
    new Worker(
      new URL("../../../web/src/components/files/office/document.worker.ts", import.meta.url),
      {
        type: "module",
      },
    ),
  pptx: () =>
    new Worker(
      new URL("../../../web/src/components/files/office/presentation.worker.ts", import.meta.url),
      { type: "module" },
    ),
  xlsx: () =>
    new Worker(
      new URL("../../../web/src/components/files/office/spreadsheet.worker.ts", import.meta.url),
      { type: "module" },
    ),
};
