// OfficeViewer.tsx's ExtendedOfficeViewer, DocumentPages.tsx and VirtualTablePreview.tsx: the OpenDocument, rich
// text and EPUB files the preparation worker reads (`extendedDocumentPreview.ts` in `/engine.js`'s bundle).
import DOMPurify from "/purify.mjs";
import { CHEVRON_LEFT, CHEVRON_RIGHT, action, h, notice, withTooltip } from "/pages.mjs";

/** `PREVIEW_PAGE_ROWS`: the rows of a sheet the worker sends at a time. */
const PAGE_ROWS = 32;
const ROW_HEADER = 56;

const MINUS = '<path d="M5 12h14" />';
const PLUS = '<path d="M5 12h14" /><path d="M12 5v14" />';
const ROTATE_CCW =
  '<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" /><path d="M3 3v5h5" />';
const CHEVRON_DOWN = '<path d="m6 9 6 6 6-6" />';

/** `formatAttachmentSize`, with the viewers' "0 bytes" for an empty file. */
export function formatSize(size) {
  if (size === 0) return "0 bytes";
  const kilobytes = Math.max(1, Math.ceil(size / 1024));
  return kilobytes >= 1024 ? `${(size / (1024 * 1024)).toFixed(1)} MB` : `${kilobytes} KB`;
}

/** `sanitizeDocumentHtml`: the worker's markup with only its own tags, and images only as data URLs. */
function sanitize(source) {
  const fragment = DOMPurify.sanitize(source, {
    RETURN_DOM_FRAGMENT: true,
    ALLOWED_TAGS: [
      "p",
      "h1",
      "h2",
      "h3",
      "h4",
      "h5",
      "h6",
      "div",
      "span",
      "strong",
      "b",
      "em",
      "i",
      "u",
      "s",
      "ul",
      "ol",
      "li",
      "blockquote",
      "pre",
      "code",
      "table",
      "thead",
      "tbody",
      "tr",
      "th",
      "td",
      "caption",
      "figure",
      "figcaption",
      "br",
      "hr",
      "sub",
      "sup",
      "dl",
      "dt",
      "dd",
      "img",
    ],
    ALLOWED_ATTR: ["src", "alt", "colspan", "rowspan", "data-odf-whitespace"],
  });
  for (const image of fragment.querySelectorAll("img")) {
    if (
      !/^data:image\/(png|jpeg|gif|webp);base64,[a-z\d+/]*={0,2}$/i.test(
        image.getAttribute("src") ?? "",
      )
    )
      image.remove();
  }
  const container = document.createElement("div");
  container.append(fragment);
  return container.innerHTML;
}

function svg(paths) {
  const node = h("span", { class: "select-icon", "aria-hidden": "true" });
  node.innerHTML = `<svg viewBox="0 0 24 24">${paths}</svg>`;
  return node;
}

/**
 * The bar above a multi-page file: previous and next, with a select of sheets or chapters or a "Page 2 of 5"
 * label between. `setPage` reflects the page shown.
 */
function navigationBar({ navigation, count, choices, onSelect }) {
  let page = 0;
  const previous = action(`Previous ${navigation}`, CHEVRON_LEFT, () => onSelect(page - 1));
  const next = action(`Next ${navigation}`, CHEVRON_RIGHT, () => onSelect(page + 1));
  let select;
  let label;
  if (choices) {
    select = h(
      "select",
      {
        class: "select",
        "aria-label": navigation === "sheet" ? "Sheet" : "Chapter",
        onchange: () => onSelect(Number(select.value)),
      },
      choices.map((choice, index) => h("option", { value: index }, choice)),
    );
  } else label = h("span", { class: "page-label", "aria-live": "polite" });
  const element = h(
    "div",
    { class: "navigation" },
    previous,
    select ? h("div", { class: "select-wrap" }, select, svg(CHEVRON_DOWN)) : label,
    next,
  );
  return {
    element,
    setPage(index) {
      page = index;
      previous.disabled = page === 0;
      next.disabled = page + 1 === count;
      if (select) select.value = String(page);
      else
        label.textContent = `${navigation === "slide" ? "Slide" : "Page"} ${page + 1} of ${count}`;
    },
  };
}

/** The reading frame's colours, read from the theme the app put on the page. */
function documentColors() {
  const style = getComputedStyle(document.documentElement);
  return {
    scheme: style.colorScheme === "dark" ? "dark" : "light",
    colors: ["background", "foreground", "card", "border"].map((token) =>
      style.getPropertyValue(`--${token}`).trim(),
    ),
  };
}

/** `DocumentPages`' sandboxed frame: the page's markup on a card, scripts blocked, sized by the reading zoom. */
function documentSource(html, zoom) {
  const { scheme, colors } = documentColors();
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'"><style>:root{color-scheme:${scheme}}html{background:${colors[0]};color:${colors[1]};font:${15 * zoom}px/1.65 system-ui,sans-serif}body{box-sizing:border-box;margin:20px auto;padding:32px;width:calc(100% - 40px);max-width:${860 * zoom}px;min-height:calc(100vh - 40px);display:flow-root;background:${colors[2]};overflow-wrap:anywhere}h1,h2,h3{line-height:1.25}img{max-width:100%;height:auto}table{border-collapse:collapse;min-width:100%;max-width:100%}td,th{border:1px solid ${colors[3]};padding:6px 12px;text-align:left}th{background:${colors[0]}}tr:nth-child(even){background:${colors[0]}}pre,span[data-odf-whitespace]{white-space:pre-wrap}*{scrollbar-width:thin}@media(max-width:420px){body{padding:20px;width:calc(100% - 24px);margin:12px auto}}</style></head><body>${html}</body></html>`;
}

/**
 * `ExtendedOfficeViewer` for a document: pages, slides or chapters, one at a time, with reading-size controls.
 * `refresh` repaints the page after a theme change.
 */
export function extendedDocument(result, { name, kind, size }) {
  const { pages, navigation } = result;
  const count = pages.length > 1 ? pages.length : 0;
  const sanitized = new Map();
  let page = 0;
  let zoom = 1;
  const details = h("span", { class: "reading-details", role: "status" });
  const zoomLabel = h("span", { class: "zoom-label" });
  const setZoom = (value) => {
    zoom = value;
    render();
  };
  const smaller = action("Decrease reading size", MINUS, () => setZoom(Math.max(0.5, zoom - 0.25)));
  const larger = action("Increase reading size", PLUS, () => setZoom(Math.min(2, zoom + 0.25)));
  const reset = action("Reset reading size", ROTATE_CCW, () => setZoom(1));
  const frame = h("iframe", { class: "document-frame", sandbox: "" });
  const bar =
    count > 0
      ? navigationBar({
          navigation,
          count,
          choices: navigation === "chapter" ? pages.map((item) => item.title) : null,
          onSelect: (index) => {
            page = index;
            render();
          },
        })
      : null;
  const render = () => {
    const current = pages[page];
    bar?.setPage(page);
    const label =
      navigation === "chapter"
        ? `Chapter ${page + 1} of ${pages.length}`
        : `${pages.length} ${navigation}${pages.length === 1 ? "" : "s"}`;
    details.textContent = `${label} · ${formatSize(size)}`;
    zoomLabel.textContent = `${Math.round(zoom * 100)}%`;
    smaller.disabled = zoom <= 0.5;
    larger.disabled = zoom >= 2;
    reset.disabled = zoom === 1;
    frame.title = `${name}: ${current.title}`;
    if (!sanitized.has(page)) sanitized.set(page, sanitize(current.html));
    frame.srcdoc = documentSource(sanitized.get(page), zoom);
  };
  const element = h(
    "div",
    { class: "viewer-body" },
    bar?.element,
    kind !== "epub"
      ? notice("Text and images. Layout and embedded objects may differ from the original.")
      : null,
    h("div", { class: "reading-bar" }, details, smaller, zoomLabel, larger, reset),
    frame,
  );
  render();
  return { element, refresh: render };
}

/** `columnLabel`: A, B, … Z, AA, AB, … */
function columnLabel(index) {
  let label = "";
  for (let value = index + 1; value > 0; value = Math.floor((value - 1) / 26))
    label = String.fromCharCode(65 + ((value - 1) % 26)) + label;
  return label;
}

/**
 * `VirtualTablePreview` for a sheet: lettered columns sized to the panel, numbered rows, and row pages that
 * mount as they come near and ask the worker for their cells. `receive` takes a page the worker sent.
 */
function sheetTable(sheet, { name, loadPage, signal }) {
  const scroller = h("div", { class: "sheet-scroll", tabindex: 0 });
  const table = h("div", {
    role: "table",
    class: "sheet-table",
    "aria-label": name,
    "aria-rowcount": sheet.rowCount,
    "aria-colcount": sheet.columns,
  });
  const columns = Array.from({ length: sheet.columns }, (_, column) => column);
  table.append(
    h(
      "div",
      { role: "row", class: "sheet-head" },
      h("div", { class: "sheet-corner" }),
      columns.map((column) =>
        withTooltip(
          h(
            "div",
            { role: "columnheader", class: "sheet-column", "aria-colindex": column + 1 },
            columnLabel(column),
          ),
          columnLabel(column),
        ),
      ),
    ),
  );
  const groups = [];
  const byNode = new WeakMap();
  const pending = new Set();
  const fill = (group, rows) => {
    group.filled = true;
    observer.unobserve(group.node);
    group.node.style.minHeight = "";
    group.node.replaceChildren(
      ...Array.from({ length: group.count }, (_, local) => {
        const index = group.page * PAGE_ROWS + local;
        const row = rows[local] ?? [];
        return h(
          "div",
          {
            role: "row",
            class: index % 2 ? "sheet-row striped" : "sheet-row",
            "aria-rowindex": index + 1,
          },
          h("div", { role: "rowheader", class: "sheet-number" }, String(index + 1)),
          columns.map((column) =>
            h(
              "div",
              { role: "cell", class: "sheet-cell", "aria-colindex": column + 1 },
              row[column] ?? "",
            ),
          ),
        );
      }),
    );
  };
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const group = byNode.get(entry.target);
        if (!entry.isIntersecting || !group || group.filled || pending.has(group.page)) continue;
        pending.add(group.page);
        loadPage(group.page);
      }
    },
    { root: scroller, rootMargin: "100% 0px" },
  );
  for (let page = 0; page * PAGE_ROWS < sheet.rowCount; page++) {
    const count = Math.min(PAGE_ROWS, sheet.rowCount - page * PAGE_ROWS);
    const node = h("div", { role: "rowgroup", style: { minHeight: `${count * 29}px` } });
    const group = { page, count, node, filled: false };
    groups.push(group);
    byNode.set(node, group);
    table.append(node);
    if (page === 0) fill(group, sheet.rows);
    else observer.observe(node);
  }
  scroller.append(table);
  const resize = new ResizeObserver(() => {
    const width = scroller.clientWidth;
    const column = Math.max(128, Math.min(320, (width - ROW_HEADER) / Math.min(sheet.columns, 6)));
    table.style.setProperty("--column-width", `${column}px`);
    table.style.width = `${Math.max(width, ROW_HEADER + column * sheet.columns)}px`;
  });
  resize.observe(scroller);
  signal.addEventListener(
    "abort",
    () => {
      resize.disconnect();
      observer.disconnect();
    },
    { once: true },
  );
  return {
    element: scroller,
    receive(page, rows) {
      pending.delete(page);
      const group = groups[page];
      if (group && !group.filled) fill(group, rows);
    },
  };
}

/**
 * `ExtendedOfficeViewer` for an OpenDocument workbook: one sheet at a time, its size, and its cell values.
 * `receive` takes a page of rows the worker sent for `requestPage(sheet, index)`.
 */
export function workbook(result, { name, size, requestPage, signal }) {
  const { sheets } = result;
  let page = 0;
  let table = null;
  let tableScope = null;
  const status = h("div", { class: "sheet-status", role: "status" });
  const content = h("div", { class: "viewer-body" });
  const bar = navigationBar({
    navigation: "sheet",
    count: sheets.length,
    choices: sheets.map((sheet) => sheet.name),
    onSelect: (index) => show(index),
  });
  const show = (index) => {
    page = index;
    bar.setPage(page);
    const sheet = sheets[page];
    status.textContent = `Sheet ${page + 1} of ${sheets.length} · ${sheet.rowCount.toLocaleString()} rows · ${sheet.columns.toLocaleString()} columns · ${formatSize(size)}`;
    tableScope?.abort();
    tableScope = new AbortController();
    table =
      sheet.rowCount === 0
        ? null
        : sheetTable(sheet, {
            name: `${name}: ${sheet.name}`,
            loadPage: (index) => requestPage(page, index),
            signal: tableScope.signal,
          });
    content.replaceChildren(
      notice(
        `Cell values only. Charts, images, and formatting are not shown.${sheet.truncated ? " Limited to 500 rows and 50 columns." : ""}`,
      ),
      table ? table.element : h("p", { class: "sheet-empty" }, "This sheet is empty."),
    );
  };
  signal.addEventListener("abort", () => tableScope?.abort(), { once: true });
  show(0);
  return {
    element: h("div", { class: "viewer-body" }, bar.element, status, content),
    receive(result) {
      if (result.sheet === page) table?.receive(result.index, result.rows);
    },
  };
}
