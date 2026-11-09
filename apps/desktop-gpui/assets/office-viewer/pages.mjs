// OfficePages.tsx, WordDocument.tsx and Presentation.tsx on the web's Office workers (`/engine.js`), and the
// pieces of fileSurfaceChrome.tsx the viewers share.
import {
  createWorkerClient,
  externalUrl,
  layerFont,
  lineHeight,
  maxImageSide,
  workers,
} from "/engine.js";

const TOOLTIP_DELAY_MS = 600;

/** An element with attributes, listeners (`on<event>`), styles and children; null and false props are skipped. */
export function h(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [name, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (name === "class") node.className = value;
    else if (name === "style") Object.assign(node.style, value);
    else if (name.startsWith("on")) node.addEventListener(name.slice(2), value);
    else node.setAttribute(name, value === true ? "" : String(value));
  }
  node.append(
    ...children.flat().filter((child) => child !== null && child !== undefined && child !== false),
  );
  return node;
}

function icon(node, paths) {
  node.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${paths}</svg>`;
  return node;
}

/** `FileSurfaceLoading`. */
export function loading() {
  return icon(
    h("div", { class: "loading", role: "status", "aria-label": "Loading file" }),
    '<path d="M21 12a9 9 0 1 1-6.219-8.56" />',
  );
}

/** `FileSurfaceNotice`. */
export function notice(text) {
  return h("div", { class: "notice", role: "status" }, text);
}

export function messageOf(cause, fallback) {
  return cause instanceof Error ? cause.message : fallback;
}

/** A request to the app, which acts only on the ones it accepts. */
export function post(message) {
  window.ipc?.postMessage(JSON.stringify(message));
}

/** Opens a document's link in the system browser when it is one the web would open. */
export function openLink(href) {
  const url = externalUrl(href);
  if (url) post({ type: "open", url });
}

const tooltip = document.getElementById("tooltip");
let tooltipTimer = 0;

function showTooltip(anchor, text) {
  tooltip.textContent = text;
  tooltip.hidden = false;
  const rect = anchor.getBoundingClientRect();
  const box = tooltip.getBoundingClientRect();
  const left = Math.min(
    Math.max(4, rect.left + rect.width / 2 - box.width / 2),
    window.innerWidth - box.width - 4,
  );
  const above = rect.top - box.height - 4;
  tooltip.style.left = `${left}px`;
  tooltip.style.top = `${above >= 4 ? above : rect.bottom + 4}px`;
}

function hideTooltip() {
  clearTimeout(tooltipTimer);
  tooltip.hidden = true;
}

/** A tooltip after a hover delay, or at once on keyboard focus; a press hides it. */
export function withTooltip(node, text) {
  node.addEventListener("pointerenter", () => {
    clearTimeout(tooltipTimer);
    tooltipTimer = setTimeout(() => showTooltip(node, text), TOOLTIP_DELAY_MS);
  });
  node.addEventListener("pointerleave", hideTooltip);
  node.addEventListener("pointerdown", hideTooltip);
  node.addEventListener("focus", () => {
    if (node.matches(":focus-visible")) showTooltip(node, text);
  });
  node.addEventListener("blur", hideTooltip);
  return node;
}

/** `FileSurfaceAction`: a ghost icon button with its label as tooltip. */
function action(label, paths, onPress) {
  const button = icon(h("button", { type: "button", class: "action", "aria-label": label }), paths);
  button.addEventListener("click", onPress);
  return withTooltip(button, label);
}

/**
 * The page column: pages laid out at their width up to the column's, mounted in batches, painted on canvases
 * while within a viewport of the visible area and released past it. `scrollTo` shows a page; `setPages` takes a
 * new layout of the same document.
 */
function officePages(options) {
  const { paint, label, overlay, footer, onPageChange, onError, signal } = options;
  let pages = options.pages;
  const scroller = h("div", { class: "pages-scroll", "data-office-pages": true });
  const column = h("div", { class: "page-column" });
  scroller.append(column);
  /** The mounted pages, by index. */
  const rows = [];
  const byNode = new WeakMap();
  let width = 0;
  let settled = 0;
  let mounted = 6;
  let ratio = window.devicePixelRatio || 1;
  let settleTimer = 0;
  let mountTimer = 0;

  const scaleAt = (page, at) => Math.min(1, Math.max(0, at - 48) / page.width);

  const size = (row) => {
    const page = pages[row.index];
    const scale = scaleAt(page, width);
    row.wrap.style.width = `${page.width * scale}px`;
    row.box.style.width = `${page.width * scale}px`;
    row.box.style.height = `${page.height * scale}px`;
    row.box.style.background = page.background ?? "#fff";
  };

  // Paints when the page comes near, when its raster scale or the screen's density changes, or when its page does.
  const repaint = (row, force) => {
    const raster = scaleAt(pages[row.index], settled || width);
    if (!force && raster === row.raster) return;
    row.raster = raster;
    row.painting?.abort();
    row.painting = undefined;
    if (!row.near || ratio <= 0 || raster <= 0) return;
    const controller = new AbortController();
    row.painting = controller;
    paint(row.index, row.canvas, raster, controller.signal).catch((cause) => {
      if (!controller.signal.aborted) onError(cause);
    });
  };

  const showOverlay = (row) => {
    row.overlay?.abort();
    row.overlay = undefined;
    row.layer?.remove();
    row.layer = undefined;
    if (!row.near || !overlay) return;
    const controller = new AbortController();
    row.overlay = controller;
    row.layer = overlay(row.index, controller.signal) ?? undefined;
    if (row.layer) row.box.append(row.layer);
  };

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const row = byNode.get(entry.target);
        if (!row || row.near === entry.isIntersecting) continue;
        row.near = entry.isIntersecting;
        // A page that moves away gives back its canvas's backing store.
        if (!row.near) {
          row.canvas.width = 0;
          row.canvas.height = 0;
        }
        repaint(row, true);
        showOverlay(row);
      }
    },
    { root: scroller, rootMargin: "100% 0px" },
  );

  const mount = (index) => {
    const canvas = h("canvas", { "aria-hidden": "true" });
    const box = h(
      "div",
      { class: "office-page", role: "group", "aria-label": `${label} ${index + 1}` },
      canvas,
    );
    const wrap = h("div", { class: "page-row" }, box, footer?.(index));
    const row = { index, wrap, box, canvas, near: false, raster: undefined };
    rows.push(row);
    byNode.set(box, row);
    size(row);
    column.append(wrap);
    observer.observe(box);
  };

  const unmount = (row) => {
    row.painting?.abort();
    row.overlay?.abort();
    observer.unobserve(row.box);
    row.wrap.remove();
  };

  // Yields between batches so a long document never holds the page.
  const fill = () => {
    mountTimer = 0;
    while (rows.length > pages.length) unmount(rows.pop());
    while (rows.length < Math.min(mounted, pages.length)) mount(rows.length);
    if (mounted < pages.length)
      mountTimer = setTimeout(() => {
        mounted = Math.min(pages.length, mounted + 50);
        fill();
      }, 0);
  };

  const resize = new ResizeObserver(([entry]) => {
    if (!entry) return;
    width = entry.contentRect.width;
    for (const row of rows) {
      size(row);
      repaint(row, false);
    }
    clearTimeout(settleTimer);
    settleTimer = setTimeout(() => {
      settled = width;
      for (const row of rows) repaint(row, false);
    }, 150);
  });
  resize.observe(scroller);

  let density;
  const watchDensity = () => {
    density = matchMedia(`(resolution: ${ratio}dppx)`);
    density.addEventListener("change", changeDensity, { once: true });
  };
  const changeDensity = () => {
    ratio = window.devicePixelRatio || 1;
    for (const row of rows) repaint(row, true);
    watchDensity();
  };
  watchDensity();

  scroller.addEventListener("scroll", () => {
    if (!onPageChange) return;
    if (
      scroller.scrollHeight > scroller.clientHeight &&
      scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 1
    ) {
      onPageChange(Math.min(mounted, pages.length) - 1);
      return;
    }
    const row = rows.find(
      (row) => row.box.offsetTop + row.box.offsetHeight > scroller.scrollTop + 24,
    );
    if (row) onPageChange(row.index);
  });

  signal.addEventListener(
    "abort",
    () => {
      clearTimeout(settleTimer);
      clearTimeout(mountTimer);
      density.removeEventListener("change", changeDensity);
      resize.disconnect();
      for (const row of rows) unmount(row);
      observer.disconnect();
    },
    { once: true },
  );
  fill();

  return {
    element: scroller,
    scrollTo(index, y = 0) {
      const row = rows[index];
      const page = pages[index];
      if (row && page)
        scroller.scrollTo({ top: row.box.offsetTop + y * scaleAt(page, width) - 24 });
    },
    setPages(next) {
      pages = next;
      clearTimeout(mountTimer);
      fill();
      for (const row of rows) {
        size(row);
        repaint(row, true);
        showOverlay(row);
      }
    },
  };
}

/** Draws a worker's bitmap on a page's canvas. */
function show(canvas, bitmap) {
  const context = canvas.getContext("bitmaprenderer");
  if (!context) return false;
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  context.transferFromImageBitmap(bitmap);
  return true;
}

/** Opens a kind's worker for `signal`'s lifetime; worker failures reach `fail`. */
function engineFor(start, signal, fail) {
  const worker = start();
  const engine = createWorkerClient(worker);
  worker.addEventListener("error", fail);
  worker.addEventListener("messageerror", fail);
  signal.addEventListener("abort", () => engine.close(), { once: true });
  return engine;
}

/** `WordDocument`: pages laid out a step at a time past the first, a selectable text layer, links and bookmarks. */
export function wordDocument(bytes, { onDetails, onError, signal }) {
  const root = h("div", { class: "viewer-body" }, loading());
  const fail = (cause) => {
    if (!signal.aborted) onError(cause);
  };
  const engine = engineFor(workers.docx, signal, fail);
  let column;
  const paint = async (index, canvas, scale, paintSignal) => {
    const bitmap = await engine.call(
      "paint",
      { index, scale, ratio: window.devicePixelRatio || 1 },
      { key: `paint:${index}`, signal: paintSignal },
    );
    void engine
      .call("build", { index: index + 1 }, { key: "build", signal: paintSignal })
      .catch(() => undefined);
    if (!bitmap) return;
    if (paintSignal.aborted || !show(canvas, bitmap)) bitmap.close();
  };
  const follow = (link) => {
    if (link.kind === "external") {
      openLink(link.url);
      return;
    }
    engine
      .call("bookmark", { name: link.bookmark })
      .then((target) => {
        if (target) column?.scrollTo(target.index, target.y);
      })
      .catch(fail);
  };
  const buffer = bytes.slice(0);
  engine
    .call("open", { bytes: buffer }, { transfer: [buffer], signal })
    .then(async (first) => {
      if (signal.aborted) return;
      column = officePages({
        pages: first.pages,
        paint,
        label: "Page",
        overlay: (index, layerSignal) => textLayer(engine, index, follow, layerSignal),
        onError: fail,
        signal,
      });
      root.replaceChildren(column.element);
      onDetails(`${first.pages.length}${first.loading ? "+" : ""} pages`);
      if (!first.loading) return;
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      while (!signal.aborted) {
        const step = await engine.call("layout", undefined, { signal });
        if (!step.done) continue;
        if (step.pages.length > 1000) throw new Error("Preview limited to 1,000 pages.");
        column.setPages(step.pages);
        onDetails(`${step.pages.length} pages`);
        break;
      }
    })
    .catch(fail);
  return root;
}

/** `WordTextLayer`: a page's text, transparent over its glyphs, for selection, find and links. */
function textLayer(engine, index, follow, signal) {
  const layer = h("div", {
    class: "text-layer",
    style: { fontFamily: layerFont, lineHeight: String(lineHeight) },
  });
  engine
    .call("text", { index }, { signal })
    .then((spans) => {
      if (signal.aborted) return;
      layer.replaceChildren(
        ...spans.flatMap((span) => {
          const node = h(
            "span",
            {
              dir: span.dir,
              role: span.link ? "link" : undefined,
              tabindex: span.link ? 0 : undefined,
              "aria-label": span.title,
              style: {
                left: `${span.left * 100}%`,
                top: `${span.top * 100}%`,
                fontSize: `${span.size * 100}cqw`,
                transform: span.transform,
                cursor: span.link ? "pointer" : "text",
              },
              onclick: (event) => {
                if (span.link && event.button === 0 && (getSelection()?.isCollapsed ?? true))
                  follow(span.link);
              },
              onkeydown: (event) => {
                if (span.link && event.key === "Enter") {
                  event.preventDefault();
                  follow(span.link);
                }
              },
            },
            span.text,
          );
          if (span.title) withTooltip(node, span.title);
          if (span.after === "br") return [node, h("br")];
          return span.after === "tab" ? [node, "\t"] : [node];
        }),
      );
    })
    .catch(() => undefined);
  return layer;
}

const CHEVRON_LEFT = '<path d="m15 18-6-6 6-6" />';
const CHEVRON_RIGHT = '<path d="m9 18 6-6-6-6" />';

/** `Presentation`: slides painted by the deck's worker, their notes, and slide navigation. */
export function presentation(bytes, { onDetails, onError, signal }) {
  const root = h("div", { class: "viewer-body" }, loading());
  const fail = (cause) => {
    if (!signal.aborted) onError(cause);
  };
  const engine = engineFor(workers.pptx, signal, fail);
  const failed = new Map();
  /** The failure notice of each slide whose overlay is mounted. */
  const failures = new Map();
  const setFailed = (index, value) => {
    failed.set(index, value);
    const node = failures.get(index);
    if (node) node.hidden = !value;
  };
  const paint = async (index, canvas, scale, paintSignal, again = false) => {
    const result = await engine.call(
      "paint",
      { index, density: (window.devicePixelRatio || 1) * scale },
      { key: `slide:${index}`, signal: paintSignal },
    );
    if (paintSignal.aborted || result.kind === "cancelled") {
      if (result.kind === "painted") result.bitmap.close();
      return;
    }
    setFailed(index, result.kind === "failed");
    if (result.kind === "failed") return;
    if (!show(canvas, result.bitmap)) {
      result.bitmap.close();
      throw new Error("This browser cannot draw slides.");
    }
    if (again || !result.vectors.length) return;
    const bitmaps = await Promise.all(result.vectors.map(rasterize));
    if (paintSignal.aborted) {
      bitmaps.forEach((value) => value.bitmap?.close());
      return;
    }
    await engine.call("vectors", bitmaps, {
      transfer: bitmaps.flatMap((value) => (value.bitmap ? [value.bitmap] : [])),
      signal: paintSignal,
    });
    if (!paintSignal.aborted) await paint(index, canvas, scale, paintSignal, true);
  };
  const bytesCopy = bytes.slice(0);
  engine
    .call("open", { bytes: bytesCopy }, { transfer: [bytesCopy], signal })
    .then((slides) => {
      if (slides.length === 0 || slides.length > 500)
        throw new Error("Preview limited to presentations with 1 to 500 slides.");
      if (signal.aborted) return;
      let page = 0;
      const label = h("span", { class: "slide-label", "aria-live": "polite" });
      const previous = action("Previous slide", CHEVRON_LEFT, () => select(page - 1));
      const next = action("Next slide", CHEVRON_RIGHT, () => select(page + 1));
      const setPage = (index) => {
        page = index;
        label.textContent = `Slide ${page + 1} of ${slides.length}`;
        previous.disabled = page === 0;
        next.disabled = page + 1 === slides.length;
      };
      const column = officePages({
        pages: slides,
        paint,
        label: "Slide",
        onPageChange: setPage,
        onError: fail,
        signal,
        overlay: (index, overlaySignal) => {
          const failure = h(
            "p",
            { role: "status", class: "slide-failed", hidden: !failed.get(index) },
            "This slide could not be drawn.",
          );
          failures.set(index, failure);
          overlaySignal.addEventListener("abort", () => {
            if (failures.get(index) === failure) failures.delete(index);
          });
          return h(
            "div",
            { class: "slide-overlay" },
            slides[index]?.hidden ? h("div", { class: "slide-hidden" }) : null,
            failure,
          );
        },
        footer: (index) => {
          const slide = slides[index];
          if (!slide?.notes && !slide?.hidden) return null;
          return h(
            "div",
            { class: "slide-footer" },
            slide.hidden ? h("p", {}, "Hidden slide") : null,
            slide.notes
              ? h(
                  "p",
                  { role: "note", "aria-label": "Speaker notes", class: "slide-notes" },
                  slide.notes,
                )
              : null,
          );
        },
      });
      const select = (index) => {
        setPage(index);
        column.scrollTo(index);
      };
      setPage(0);
      root.replaceChildren(h("div", { class: "subheader" }, previous, label, next), column.element);
      onDetails(`${slides.length} slides`);
    })
    .catch(fail);
  return root;
}

/** An SVG picture the worker cannot decode, rasterized here at the size the slide needs. */
async function rasterize(request) {
  const url = URL.createObjectURL(request.blob);
  const image = new Image();
  image.src = url;
  try {
    await image.decode();
    const shrink = Math.min(1, maxImageSide / Math.max(image.naturalWidth, image.naturalHeight));
    const size = request.size ?? {
      width: Math.max(1, Math.round(image.naturalWidth * shrink)),
      height: Math.max(1, Math.round(image.naturalHeight * shrink)),
    };
    return {
      asset: request.asset,
      bitmap: await createImageBitmap(image, {
        resizeWidth: size.width,
        resizeHeight: size.height,
        resizeQuality: "high",
      }),
    };
  } catch {
    return { asset: request.asset, bitmap: undefined };
  } finally {
    URL.revokeObjectURL(url);
  }
}
