// PdfViewer.tsx on PDF.js: page navigation, zoom, fit width, find, and the reading position,
// which the app keeps per file through `window.ipc` and hands back in the page's state.
import { getDocument, GlobalWorkerOptions, PDFWorker, TextLayer } from "/pdf.min.mjs";
import {
  currentPdfPage,
  pdfFindIncludes,
  pdfFindMatchRuns,
  pdfPageOffsets,
  pdfPageScrollTop,
  pdfPageText,
  pdfPageWidth,
  pdfRestoredScrollTop,
  PDF_MAX_ZOOM,
  PDF_MIN_ZOOM,
  stepPdfZoom,
  visiblePdfPages,
} from "/layout.mjs";

const MAX_PAGES = 2000;
const MAX_FIND_TEXT = 4 * 1024 * 1024;
const LOAD_TIMEOUT_MS = 30_000;
const FIND_DELAY_MS = 250;
const TOOLTIP_DELAY_MS = 600;
const POST_DELAY_MS = 100;

GlobalWorkerOptions.workerSrc = "/pdf.worker.min.mjs";

const byId = (id) => document.getElementById(id);
const ui = {
  pageLabel: byId("page-label"),
  previous: byId("previous"),
  next: byId("next"),
  zoomOut: byId("zoom-out"),
  zoomLabel: byId("zoom-label"),
  zoomIn: byId("zoom-in"),
  fit: byId("fit"),
  find: byId("find"),
  findStatus: byId("find-status"),
  nextMatch: byId("next-match"),
  scroll: byId("scroll"),
  loading: byId("loading"),
  failure: byId("failure"),
  failureMessage: byId("failure-message"),
  retry: byId("retry"),
  pages: byId("pages"),
  tooltip: byId("tooltip"),
  theme: byId("theme"),
};

const state = JSON.parse(byId("state").textContent);
/** The reading position the app kept for this file: a view plus the width and page shapes it was taken at. */
let saved = state.saved ?? null;
let initialView = saved ?? { page: 0, offset: 0, zoom: null, left: 0 };

let pdf = null;
let error = null;
let worker;
let task;
let controller;
let loadTimer = 0;
let generation = 0;
let ratios = [];
let zoom = initialView.zoom;
let pageWidth = 612;
let viewport = { width: 600, height: 800 };
let scrollTop = 0;
let scale = 1;
let width = 1;
let offsets = [16];
let query = "";
let matches = [];
let finding = false;
let findError = null;
let match = 0;
let findRun = 0;
let findTimer = 0;
let lastView = null;
const mounted = new Map();

function setText(node, text) {
  if (node.textContent !== text) node.textContent = text;
}

function messageOf(cause, fallback) {
  return cause instanceof Error && cause.message ? cause.message : fallback;
}

// One worker per page: without it, PDF.js starts a worker and compiles its script for every load.
function openPdf(bytes) {
  worker ??= new PDFWorker();
  const base = new URL("/", location.href).href;
  return getDocument({
    worker,
    data: new Uint8Array(bytes),
    useSystemFonts: true,
    useWorkerFetch: false,
    cMapUrl: `${base}cmaps/`,
    cMapPacked: true,
    standardFontDataUrl: `${base}standard_fonts/`,
    wasmUrl: `${base}wasm/`,
    canvasMaxAreaInBytes: 32 * 1024 * 1024,
    useWasm: true,
    isEvalSupported: false,
  });
}

let pending = {};
let posted = {};
let postTimer = 0;

/** Reports the position to the app, at most every 100 ms; page shapes only when they change. */
function post(type, message) {
  pending[type] = message;
  if (!postTimer) postTimer = setTimeout(flushPosts, POST_DELAY_MS);
}

function flushPosts() {
  postTimer = 0;
  for (const [type, message] of Object.entries(pending)) {
    const body = JSON.stringify({ type, ...message });
    if (posted[type] !== body) {
      posted[type] = body;
      window.ipc?.postMessage(body);
    }
  }
  pending = {};
}

function relayout() {
  ({ scale, width } = pdfPageWidth(viewport.width, pageWidth, zoom));
  offsets = pdfPageOffsets(ratios, width);
  ui.pages.style.height = `max(${offsets.at(-1) ?? 0}px, calc(100% + ${(offsets.at(-2) ?? 16) - 16}px))`;
  ui.pages.style.minWidth = `${width + 32}px`;
}

function setRatios(next) {
  ratios = next;
  post("ratios", { ratios });
  relayout();
}

let syncFrame = 0;
function scheduleSync() {
  syncFrame ||= requestAnimationFrame(() => {
    syncFrame = 0;
    sync();
  });
}

function sync() {
  const current = pdf ? currentPdfPage(offsets, ratios.length, scrollTop) : 0;
  setText(ui.pageLabel, pdf ? `${current + 1} / ${pdf.numPages} pages` : "PDF");
  ui.previous.disabled = !pdf || current === 0;
  ui.next.disabled = !pdf || current + 1 >= pdf.numPages;
  ui.zoomOut.disabled = scale <= PDF_MIN_ZOOM;
  ui.zoomIn.disabled = scale >= PDF_MAX_ZOOM;
  setText(ui.zoomLabel, zoom === null ? "Fit width" : `${Math.round(scale * 100)}%`);
  ui.loading.hidden = pdf !== null || error !== null;
  ui.failure.hidden = error === null;
  ui.pages.hidden = pdf === null || error !== null;
  setText(ui.failureMessage, error ?? "");
  syncPages();
  saveView();
}

function syncPages() {
  const visible =
    pdf && error === null ? visiblePdfPages(ratios, offsets, scrollTop, viewport.height) : [];
  const keep = new Set(visible.map((page) => page.index));
  for (const [index, page] of mounted) {
    if (!keep.has(index)) {
      page.dispose();
      mounted.delete(index);
    }
  }
  const left = Math.max(16, (viewport.width - width) / 2);
  for (const { index, ratio } of visible) {
    let page = mounted.get(index);
    if (!page) {
      page = createPage(index);
      mounted.set(index, page);
      ui.pages.append(page.element);
    }
    const style = page.element.style;
    style.top = `${offsets[index]}px`;
    style.left = `${left}px`;
    style.width = `${width}px`;
    style.height = `${width * ratio}px`;
    if (page.width !== width) page.render(width);
  }
}

function disposePages() {
  for (const page of mounted.values()) page.dispose();
  mounted.clear();
}

function createPage(index) {
  const element = document.createElement("div");
  element.className = "page";
  const canvas = document.createElement("canvas");
  canvas.setAttribute("aria-label", `Page ${index + 1}`);
  const text = document.createElement("div");
  text.className = "text-layer";
  element.append(canvas, text);
  // PDF.js fills the text layer asynchronously; highlight its spans as they arrive.
  const observer = new MutationObserver(() => markMatches(text));
  observer.observe(text, { childList: true, subtree: true });
  let cancel = () => {};
  const page = {
    element,
    text,
    width: 0,
    render(renderWidth) {
      cancel();
      page.width = renderWidth;
      const source = pdf;
      let cancelled = false;
      let paint;
      let layer;
      let loaded;
      (async () => {
        const proxy = await source.getPage(index + 1);
        loaded = proxy;
        if (cancelled) {
          proxy.cleanup();
          return;
        }
        const natural = proxy.getViewport({ scale: 1 });
        onSize(index, natural.height / natural.width);
        const pageViewport = proxy.getViewport({ scale: renderWidth / natural.width });
        const ratio = Math.min(
          2,
          window.devicePixelRatio || 1,
          Math.sqrt((8 * 1024 * 1024) / (pageViewport.width * pageViewport.height)),
        );
        canvas.width = Math.ceil(pageViewport.width * ratio);
        canvas.height = Math.ceil(pageViewport.height * ratio);
        paint = proxy.render({
          canvas,
          viewport: pageViewport,
          transform: [ratio, 0, 0, ratio, 0, 0],
          background: "white",
        });
        text.replaceChildren();
        text.style.setProperty("--total-scale-factor", String(pageViewport.scale));
        layer = new TextLayer({
          container: text,
          viewport: pageViewport,
          textContentSource: proxy.streamTextContent(),
        });
        await Promise.all([paint.promise, layer.render()]);
      })().catch((cause) => {
        if (!cancelled) fail(messageOf(cause, "This PDF page could not render."));
      });
      cancel = () => {
        cancelled = true;
        paint?.cancel();
        layer?.cancel();
        // Release operator lists when virtual pages leave the viewport.
        void Promise.allSettled([paint?.promise]).then(() => loaded?.cleanup());
      };
    },
    dispose() {
      cancel();
      observer.disconnect();
      element.remove();
    },
  };
  return page;
}

function onSize(index, ratio) {
  if (ratios[index] === ratio) return;
  setRatios(ratios.map((value, i) => (i === index ? ratio : value)));
  scheduleSync();
}

function markMatches(text) {
  const elements = Array.from(text.querySelectorAll('span[role="presentation"], br'));
  const hits = pdfFindMatchRuns(
    elements.map((element) => (element.tagName === "BR" ? "\n" : (element.textContent ?? ""))),
    query,
  );
  elements.forEach((element, index) => element.classList.toggle("pdf-find-match", hits[index]));
}

function saveView() {
  if (!pdf) return;
  const top = ui.scroll.scrollTop;
  const page = currentPdfPage(offsets, ratios.length, top);
  lastView = {
    page,
    offset: Math.max(0, top - (offsets[page] - 16)),
    zoom,
    left: ui.scroll.scrollLeft,
  };
  post("view", { ...lastView, width });
}

function fail(message) {
  error = message;
  disposePages();
  sync();
}

async function load() {
  const run = ++generation;
  controller?.abort();
  clearTimeout(loadTimer);
  void task?.destroy();
  task = undefined;
  disposePages();
  pdf = null;
  error = null;
  sync();
  const signal = (controller = new AbortController()).signal;
  loadTimer = setTimeout(() => {
    fail("This PDF took too long to load. Try again.");
    controller.abort();
    void task?.destroy();
  }, LOAD_TIMEOUT_MS);
  try {
    const response = await fetch(`/document.pdf?load=${run}`, { signal, cache: "no-store" });
    if (!response.ok) {
      throw new Error(
        (await response.text()) || "This file is unavailable or too large to preview.",
      );
    }
    const bytes = await response.arrayBuffer();
    signal.throwIfAborted();
    task = openPdf(bytes);
    const opened = await task.promise;
    if (opened.numPages > MAX_PAGES) {
      throw new Error("Preview limited to 2,000 pages. Save this PDF to read it in full.");
    }
    const first = (await opened.getPage(1)).getViewport({ scale: 1 });
    const sizes = await Promise.all(
      Array.from({ length: Math.min(initialView.page + 1, opened.numPages) }, async (_, index) => {
        const size = (await opened.getPage(index + 1)).getViewport({ scale: 1 });
        return size.height / size.width;
      }),
    );
    if (signal.aborted) return;
    pageWidth = first.width;
    setRatios(
      saved?.ratios?.length === opened.numPages
        ? saved.ratios
        : Array.from(
            { length: opened.numPages },
            (_, index) => sizes[index] ?? first.height / first.width,
          ),
    );
    pdf = opened;
    restoreView();
    scheduleFind();
  } catch (cause) {
    if (run === generation && !signal.aborted) fail(messageOf(cause, "Unable to open PDF."));
  } finally {
    if (run === generation) clearTimeout(loadTimer);
  }
}

function restoreView() {
  sync();
  ui.scroll.scrollTo({
    top: pdfRestoredScrollTop(ratios, width, initialView, saved?.width),
    left: initialView.left,
  });
  scrollTop = ui.scroll.scrollTop;
  sync();
}

function showPage(index) {
  ui.scroll.scrollTo({ top: pdfPageScrollTop(offsets, index) });
}

function changeZoom(value) {
  const current = currentPdfPage(offsets, ratios.length, ui.scroll.scrollTop);
  const anchor = { page: current, offset: ui.scroll.scrollTop - (offsets[current] - 16), width };
  zoom = value;
  relayout();
  if (pdf && anchor.width !== width) {
    ui.scroll.scrollTo({ top: offsets[anchor.page] - 16 + (anchor.offset * width) / anchor.width });
  }
  scrollTop = ui.scroll.scrollTop;
  sync();
}

function syncFind() {
  ui.findStatus.hidden = !query;
  ui.nextMatch.hidden = !query;
  setText(
    ui.findStatus,
    finding ? "Searching…" : (findError ?? `${matches.length ? match + 1 : 0}/${matches.length}`),
  );
  ui.nextMatch.disabled = !matches.length;
}

function scheduleFind() {
  const run = ++findRun;
  clearTimeout(findTimer);
  if (!pdf || !query.trim()) {
    finding = false;
    syncFind();
    return;
  }
  syncFind();
  const source = pdf;
  const needle = query;
  findTimer = setTimeout(() => {
    finding = true;
    findError = null;
    matches = [];
    match = 0;
    syncFind();
    (async () => {
      const found = [];
      const sizes = [];
      let size = 0;
      for (let index = 0; index < source.numPages; index++) {
        if (run !== findRun) return;
        const page = await source.getPage(index + 1);
        const pageSize = page.getViewport({ scale: 1 });
        sizes.push(pageSize.height / pageSize.width);
        const content = await page.getTextContent();
        const text = pdfPageText(content.items);
        size += text.length;
        if (size > MAX_FIND_TEXT) {
          throw new Error(
            "This PDF has too much text to search. Save it to search in another app.",
          );
        }
        if (pdfFindIncludes(text, needle)) found.push(index);
      }
      if (run !== findRun) return;
      setRatios(sizes);
      matches = found;
      if (found.length) showPage(found[0]);
      sync();
    })()
      .catch((cause) => {
        if (run === findRun) findError = messageOf(cause, "Could not search this PDF.");
      })
      .finally(() => {
        if (run !== findRun) return;
        finding = false;
        syncFind();
      });
  }, FIND_DELAY_MS);
}

function resetFind() {
  ui.find.value = "";
  query = "";
  matches = [];
  findError = null;
  match = 0;
  scheduleFind();
}

let tooltipTimer = 0;
function showTooltip(button) {
  const tooltip = ui.tooltip;
  setText(tooltip, button.getAttribute("aria-label") ?? "");
  tooltip.hidden = false;
  const anchor = button.getBoundingClientRect();
  const box = tooltip.getBoundingClientRect();
  const left = Math.min(
    Math.max(4, anchor.left + anchor.width / 2 - box.width / 2),
    window.innerWidth - box.width - 4,
  );
  const above = anchor.top - box.height - 4;
  tooltip.style.left = `${left}px`;
  tooltip.style.top = `${above >= 4 ? above : anchor.bottom + 4}px`;
}

function hideTooltip() {
  clearTimeout(tooltipTimer);
  ui.tooltip.hidden = true;
}

for (const button of document.querySelectorAll(".action")) {
  button.addEventListener("pointerenter", () => {
    clearTimeout(tooltipTimer);
    tooltipTimer = setTimeout(() => showTooltip(button), TOOLTIP_DELAY_MS);
  });
  button.addEventListener("pointerleave", hideTooltip);
  button.addEventListener("pointerdown", hideTooltip);
  button.addEventListener("focus", () => {
    if (button.matches(":focus-visible")) showTooltip(button);
  });
  button.addEventListener("blur", hideTooltip);
}

ui.previous.addEventListener("click", () => {
  showPage(currentPdfPage(offsets, ratios.length, scrollTop) - 1);
});
ui.next.addEventListener("click", () => {
  showPage(currentPdfPage(offsets, ratios.length, scrollTop) + 1);
});
ui.zoomOut.addEventListener("click", () => changeZoom(stepPdfZoom(scale, -1)));
ui.zoomIn.addEventListener("click", () => changeZoom(stepPdfZoom(scale, 1)));
ui.fit.addEventListener("click", () => {
  changeZoom(null);
  ui.scroll.scrollTo({ left: 0 });
});
ui.find.addEventListener("input", () => {
  query = ui.find.value;
  matches = [];
  findError = null;
  for (const page of mounted.values()) markMatches(page.text);
  scheduleFind();
});
ui.nextMatch.addEventListener("click", () => {
  match = (match + 1) % matches.length;
  showPage(matches[match]);
  syncFind();
});
ui.retry.addEventListener("click", () => void load());
ui.scroll.addEventListener("scroll", () => {
  scrollTop = ui.scroll.scrollTop;
  sync();
});
// Ctrl/Cmd+F finds in the document rather than opening the engine's own find bar.
document.addEventListener("keydown", (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "f") {
    event.preventDefault();
    event.stopPropagation();
    ui.find.focus();
  }
});
new ResizeObserver(() => {
  viewport = { width: ui.scroll.clientWidth, height: ui.scroll.clientHeight };
  relayout();
  sync();
}).observe(ui.scroll);

window.t3Pdf = {
  /** A new revision of the file: read it again from where the reader is. */
  reload() {
    initialView = lastView ?? initialView;
    saved = null;
    zoom = initialView.zoom;
    resetFind();
    void load();
  },
  theme(css) {
    setText(ui.theme, css);
  },
};

document.title = state.title;
ui.scroll.setAttribute("aria-label", state.title);
viewport = { width: ui.scroll.clientWidth, height: ui.scroll.clientHeight };
relayout();
void load();
