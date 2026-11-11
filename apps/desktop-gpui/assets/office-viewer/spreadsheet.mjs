// Spreadsheet.tsx on the web's workbook worker (`/engine.js`): painted frames of the active sheet with its styles,
// merges and charts, Excel's selection, keyboard and pointer navigation, links, copy, and the sheet tabs.
import {
  cellAtPoint,
  columnName,
  contains,
  coverMerges,
  createWorkerClient,
  extendTo,
  headerHeight,
  hyperlinkAtCell,
  lastCell,
  normalizeRange,
  paintCells,
  paintHeaders,
  paintSelection,
  parseHyperlinkLocation,
  safeExternalHyperlink,
  selectionAt,
  selectionKeyReducer,
  settleMerges,
  shiftGrid,
  skipHidden,
  workers,
} from "/engine.js";
import { h, loading, notice, openLink, post } from "/pages.mjs";

const isSuperseded = (cause) => cause?._tag === "SupersededError";

export function spreadsheet(bytes, { onDetails, onError, signal }) {
  const root = h("div", { class: "viewer-body" }, loading());
  const fail = (cause) => {
    if (!signal.aborted) onError(cause);
  };
  const worker = workers.xlsx();
  const engine = createWorkerClient(worker);
  worker.addEventListener("error", fail);
  worker.addEventListener("messageerror", fail);
  let opened;
  let picking = null;
  let grid = null;
  const area = h("div", { class: "viewer-body" });
  const errorSlot = h("div", { hidden: true });
  const tabs = h("div", { role: "tablist", "aria-label": "Sheets", class: "tabs" });
  const buttons = [];

  signal.addEventListener(
    "abort",
    () => {
      picking?.abort();
      grid?.abort();
      engine.close();
    },
    { once: true },
  );

  const setError = (text) => {
    errorSlot.replaceChildren(...(text ? [notice(text)] : []));
    errorSlot.hidden = !text;
  };

  const syncTabs = () => {
    opened.tabs.forEach((tab, index) => {
      const selected = tab.index === opened.layout.info.activeSheet;
      buttons[index].setAttribute("aria-selected", String(selected));
      buttons[index].tabIndex = selected ? 0 : -1;
    });
  };

  // A grid per sheet, as the web keys it by the active sheet.
  const mountGrid = () => {
    grid?.abort();
    grid = new AbortController();
    const navigate = (name, cell) => {
      const tab = opened.tabs.find((tab) => tab.name.toLowerCase() === name.toLowerCase());
      if (tab) show(tab.index, cell);
    };
    const { element, start } = spreadsheetGrid(
      engine,
      opened.layout,
      opened.target,
      navigate,
      grid.signal,
    );
    area.replaceChildren(element);
    start();
    syncTabs();
  };

  const show = (sheet, target) => {
    if (!opened) return;
    picking?.abort();
    if (sheet === opened.layout.info.activeSheet) return;
    const controller = new AbortController();
    picking = controller;
    engine
      .call("show", { sheet }, { signal: controller.signal })
      .then((layout) => {
        if (signal.aborted) return;
        setError("");
        opened = { ...opened, layout, target };
        mountGrid();
      })
      .catch(() => {
        if (!controller.signal.aborted && !signal.aborted)
          setError("This sheet could not be opened.");
      });
  };

  tabs.addEventListener("keydown", (event) => {
    const count = opened.tabs.length;
    const current = opened.tabs.findIndex((tab) => tab.index === opened.layout.info.activeSheet);
    const next =
      event.key === "ArrowRight"
        ? (current + 1) % count
        : event.key === "ArrowLeft"
          ? (current + count - 1) % count
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? count - 1
              : -1;
    if (next < 0) return;
    event.preventDefault();
    const tab = opened.tabs[next];
    if (tab) show(tab.index);
    buttons[next]?.focus();
  });

  const buffer = bytes.slice(0);
  engine
    .call("open", { bytes: buffer }, { transfer: [buffer], signal })
    .then((value) => {
      if (!value) throw new Error("This workbook has no visible sheets.");
      if (signal.aborted) return;
      opened = { ...value, target: undefined };
      for (const tab of value.tabs)
        buttons.push(
          h(
            "button",
            { type: "button", role: "tab", class: "tab", onclick: () => show(tab.index) },
            tab.name,
          ),
        );
      tabs.replaceChildren(...buttons);
      root.replaceChildren(errorSlot, area, tabs);
      mountGrid();
      onDetails(`${value.tabs.length} sheets`);
    })
    .catch(fail);
  return root;
}

/** `SpreadsheetGrid`: one sheet's scroll area, painted from the worker's frames. `start` runs once it is in the page. */
function spreadsheetGrid(engine, layout, initialCell, onNavigate, signal) {
  const info = layout.info;
  const sheet = info.activeSheet;
  const target = h("canvas");
  const space = h("div", { class: "grid-spacer", "aria-hidden": "true" });
  const reader = h("div", { class: "sr-only" });
  const root = h(
    "div",
    {
      dir: "ltr",
      tabindex: 0,
      role: "grid",
      class: "grid",
      "aria-label": info.sheetNames[sheet],
      "aria-readonly": "true",
      "aria-multiselectable": "true",
      "aria-rowcount": lastCell.row + 1,
      "aria-colcount": lastCell.col + 1,
      "aria-activedescendant": "office-cell-0-0",
    },
    space,
    h("div", { class: "grid-canvas", "aria-hidden": "true" }, target),
    reader,
  );
  const status = h("div", { role: "status", class: "grid-notice", hidden: true });
  const element = h("div", { class: "sheet-area" }, root, status);

  const setNotice = (text) => {
    status.textContent = text;
    status.hidden = !text;
  };
  const setReader = (cells) => {
    const rows = [...new Set(cells.map((cell) => cell.row))];
    reader.replaceChildren(
      ...rows.map((row) =>
        h(
          "div",
          { role: "row", "aria-rowindex": row + 1 },
          cells
            .filter((cell) => cell.row === row)
            .map((cell) =>
              h(
                "span",
                {
                  role: "gridcell",
                  id: `office-cell-${cell.row}-${cell.col}`,
                  "aria-colindex": cell.col + 1,
                  "aria-label": `${columnName(cell.col)}${cell.row + 1}`,
                },
                cell.text,
              ),
            ),
        ),
      ),
    );
  };

  const start = () => {
    const context = target.getContext("2d");
    if (!context) return;
    const frozen = { rows: info.frozenRows, cols: info.frozenCols };
    const headers = { width: 44, height: headerHeight };
    let selection = selectionAt(initialCell ?? { row: 0, col: 0 });
    let painted;
    let grid;
    let raf = 0;
    let requested = "";
    let sent = 0;
    let received = 0;
    let drag = false;
    let header;
    let pressed;
    let end;
    let left = info.initialScrollX;
    let top = info.initialScrollY;
    let reachX = 0;
    let reachY = 0;
    let readController;
    const range = () => coverMerges(normalizeRange(selection), layout.merges);
    const at = (cell) => {
      const merge = layout.merges.find((item) => contains(item, cell));
      return selectionAt(merge ? { row: merge.top, col: merge.left } : cell);
    };
    const schedule = () => {
      if (raf || signal.aborted) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        paint();
      });
    };
    const resizeArea = () => {
      const width = Math.min(
        (end?.x ?? Infinity) + (painted?.frame.frozen.width ?? 0),
        Math.max(info.contentWidth, layout.chartWidth, reachX, left + root.clientWidth) +
          root.clientWidth,
      );
      const height = Math.min(
        (end?.y ?? Infinity) + (painted?.frame.frozen.height ?? 0),
        Math.max(info.contentHeight, layout.chartHeight, reachY, top + root.clientHeight) +
          root.clientHeight,
      );
      space.style.width = `${headers.width + width}px`;
      space.style.height = `${headers.height + height}px`;
    };
    const paint = () => {
      const ratio = window.devicePixelRatio || 1;
      const width = root.clientWidth;
      const height = root.clientHeight;
      const size = { width: width - headers.width, height: height - headers.height };
      if (size.width <= 0 || size.height <= 0) return;
      left = root.scrollLeft;
      top = root.scrollTop;
      resizeArea();
      const input = {
        sheet,
        x: left,
        y: top,
        ...size,
        ratio,
        offset: { x: 0, y: 0 },
        wider: undefined,
      };
      const key = JSON.stringify(input);
      if (key !== requested) {
        requested = key;
        sent += 1;
        const order = sent;
        engine
          .call("frame", input, { key: "frame", signal })
          .then((reply) => {
            if (order < received || signal.aborted) {
              reply?.bitmap.close();
              return;
            }
            received = order;
            if (!reply) {
              setNotice("These cells could not be drawn. Try scrolling to another area.");
              return;
            }
            setNotice("");
            painted?.frame.bitmap.close();
            painted = { request: input, frame: reply };
            end = reply.end;
            schedule();
          })
          .catch((cause) => {
            if (!isSuperseded(cause) && !signal.aborted)
              setNotice("These cells could not be drawn.");
          });
      }
      if (
        target.width !== Math.round(width * ratio) ||
        target.height !== Math.round(height * ratio)
      ) {
        target.width = Math.round(width * ratio);
        target.height = Math.round(height * ratio);
        target.style.width = `${width}px`;
        target.style.height = `${height}px`;
      }
      if (!painted) {
        context.fillStyle = "#fff";
        context.fillRect(0, 0, target.width, target.height);
        return;
      }
      paintCells(context, painted, { ratio, headers, x: left, y: top, ...size });
      grid =
        painted.frame.grid &&
        shiftGrid(painted.frame.grid, frozen, painted.frame.frozen, {
          x: painted.frame.viewport.x - left,
          y: painted.frame.viewport.y - top,
        });
      if (!grid) return;
      const anchor = selection.anchor;
      paintSelection(context, {
        grid,
        ratio,
        headers,
        ...size,
        range: range(),
        active: coverMerges(
          { top: anchor.row, left: anchor.col, bottom: anchor.row, right: anchor.col },
          layout.merges,
        ),
      });
      paintHeaders(context, { grid, ratio, headers, width, height, selected: range() });
    };
    const read = () => {
      const focus = selection.focus;
      root.setAttribute("aria-activedescendant", `office-cell-${focus.row}-${focus.col}`);
      readController?.abort();
      readController = new AbortController();
      const first = Math.max(0, focus.row - 5);
      const start = Math.max(0, focus.col - 3);
      const cells = {
        top: first,
        left: start,
        bottom: Math.min(lastCell.row, first + 10),
        right: Math.min(lastCell.col, start + 6),
      };
      engine
        .call("cells", { sheet, range: cells }, { key: "reader", signal: readController.signal })
        .then((rows) => {
          if (!signal.aborted)
            setReader(
              rows.flatMap((row, r) =>
                row.map((text, c) => ({ row: first + r, col: start + c, text })),
              ),
            );
        })
        .catch(() => undefined);
    };
    const select = (next) => {
      selection = next;
      read();
      schedule();
    };
    const reveal = () => {
      const cell = selection.focus;
      engine
        .call("position", { sheet, cell }, { key: "position", signal })
        .then((position) => {
          if (!position || signal.aborted) return;
          const panes = painted?.frame.frozen ?? { width: 0, height: 0 };
          const place = (start, next, current, visible) =>
            start < current ? start : next > current + visible ? next - visible : current;
          const x =
            cell.col < info.frozenCols
              ? left
              : place(
                  position.start.x,
                  position.next.x,
                  left,
                  root.clientWidth - headers.width - panes.width,
                );
          const y =
            cell.row < info.frozenRows
              ? top
              : place(
                  position.start.y,
                  position.next.y,
                  top,
                  root.clientHeight - headers.height - panes.height,
                );
          reachX = Math.max(reachX, x + root.clientWidth);
          reachY = Math.max(reachY, y + root.clientHeight);
          resizeArea();
          root.scrollTo({ left: Math.max(0, x), top: Math.max(0, y) });
          schedule();
        })
        .catch(() => undefined);
    };
    const hit = (event) => {
      if (!grid) return undefined;
      const rect = target.getBoundingClientRect();
      const x = event.clientX - rect.left - headers.width;
      const y = event.clientY - rect.top - headers.height;
      const frame = painted?.frame;
      if (
        frame &&
        ((x >= frame.frozen.width && x < frame.frozen.width + frame.viewport.x - left) ||
          (y >= frame.frozen.height && y < frame.frozen.height + frame.viewport.y - top))
      )
        return undefined;
      const cell = cellAtPoint(
        grid,
        Math.max(0, Math.min(x, root.clientWidth - headers.width - 1)),
        Math.max(0, Math.min(y, root.clientHeight - headers.height - 1)),
      );
      return cell ? { cell, x, y } : undefined;
    };
    const pointerDown = (event) => {
      if (event.button !== 0) return;
      const found = hit(event);
      if (!found) return;
      root.focus({ preventScroll: true });
      root.setPointerCapture(event.pointerId);
      drag = true;
      header = found.x < 0 ? (found.y < 0 ? "all" : "row") : found.y < 0 ? "column" : undefined;
      pressed = header ? undefined : found.cell;
      if (found.x < 0 && found.y < 0)
        select({ anchor: { row: lastCell.row, col: lastCell.col }, focus: { row: 0, col: 0 } });
      else if (found.x < 0)
        select({
          anchor: { row: found.cell.row, col: lastCell.col },
          focus: { row: found.cell.row, col: 0 },
        });
      else if (found.y < 0)
        select({
          anchor: { row: lastCell.row, col: found.cell.col },
          focus: { row: 0, col: found.cell.col },
        });
      else select(event.shiftKey ? extendTo(selection, found.cell) : at(found.cell));
    };
    const pointerMove = (event) => {
      if (!drag) return;
      const found = hit(event);
      if (found && header !== "all")
        select(
          extendTo(selection, {
            row: header === "column" ? 0 : found.cell.row,
            col: header === "row" ? 0 : found.cell.col,
          }),
        );
    };
    const pointerUp = (event) => {
      drag = false;
      const found = hit(event);
      if (!found || !pressed || found.cell.row !== pressed.row || found.cell.col !== pressed.col)
        return;
      const cell = at(found.cell).anchor;
      const list = painted?.frame;
      if (!list) return;
      const link = hyperlinkAtCell(
        { width: 0, height: 0, commands: [], hyperlinks: [...list.hyperlinks] },
        cell.row,
        cell.col,
      );
      if (!link) return;
      const url = safeExternalHyperlink(link);
      if (url) {
        openLink(url);
        return;
      }
      const internal =
        link.location && parseHyperlinkLocation(link.location, info.sheetNames[sheet] ?? "");
      if (!internal) return;
      if (internal.sheetName.toLowerCase() !== (info.sheetNames[sheet] ?? "").toLowerCase())
        onNavigate(internal.sheetName, { row: internal.row, col: internal.col });
      else {
        select(at({ row: internal.row, col: internal.col }));
        reveal();
      }
    };
    const copy = () => {
      const used = layout.used;
      const selected = range();
      const clipped = {
        ...selected,
        bottom: Math.min(selected.bottom, Math.max(selected.top, used?.bottom ?? 0)),
        right: Math.min(selected.right, Math.max(selected.left, used?.right ?? 0)),
      };
      if ((clipped.bottom - clipped.top + 1) * (clipped.right - clipped.left + 1) > 200_000) {
        setNotice("Select fewer than 200,000 cells to copy.");
        return;
      }
      engine
        .call("copy", { sheet, range: clipped }, { signal })
        .then((text) => {
          if (text === undefined) throw new Error("No cells");
          // The app writes the clipboard: a custom-protocol page is not trusted with it on every platform.
          post({ type: "copy", text });
        })
        .catch(() => {
          if (!signal.aborted) setNotice("The selected cells could not be copied.");
        });
    };
    const key = (event) => {
      const command = event.ctrlKey || event.metaKey;
      if (command && event.key.toLowerCase() === "c") {
        event.preventDefault();
        copy();
        return;
      }
      if (event.key === "Tab" || event.altKey || event.isComposing) return;
      const action = selectionKeyReducer(selection, event, {
        rows: Math.max((layout.used?.bottom ?? 0) + 1, selection.focus.row + 1),
        cols: Math.max((layout.used?.right ?? 0) + 1, selection.focus.col + 1),
        rowsPerPage: Math.max(1, Math.floor((root.clientHeight - headerHeight) / 20)),
      });
      if (action.type !== "move") return;
      event.preventDefault();
      select(
        skipHidden(
          selection.focus,
          settleMerges(selection, action.selection, layout.merges),
          layout.hidden,
        ),
      );
      reveal();
    };
    root.addEventListener("scroll", schedule);
    root.addEventListener("keydown", key);
    root.addEventListener("pointerdown", pointerDown);
    root.addEventListener("pointermove", pointerMove);
    root.addEventListener("pointerup", pointerUp);
    root.addEventListener("pointercancel", () => {
      drag = false;
    });
    const resize = new ResizeObserver(schedule);
    resize.observe(root);
    window.addEventListener("resize", schedule);
    signal.addEventListener(
      "abort",
      () => {
        readController?.abort();
        cancelAnimationFrame(raf);
        resize.disconnect();
        window.removeEventListener("resize", schedule);
        painted?.frame.bitmap.close();
      },
      { once: true },
    );
    resizeArea();
    root.scrollTo({ left, top });
    if (initialCell) {
      selection = at(initialCell);
      reveal();
    }
    read();
    schedule();
  };

  return { element, start };
}
