// OpenXmlOfficeViewer.tsx: reads the file, bounds its package in the preparation worker, and opens it in its
// kind's viewer, which the app reloads for a new revision and restyles for a theme change.
import { OFFICE_PREVIEW_MAX_BYTES, workers } from "/engine.js";
import { h, loading, messageOf, notice, presentation, wordDocument } from "/pages.mjs";
import { spreadsheet } from "/spreadsheet.mjs";

const PREPARE_TIMEOUT_MS = 30_000;

const state = JSON.parse(document.getElementById("state").textContent);
const root = document.getElementById("root");
const theme = document.getElementById("theme");
const viewers = { docx: wordDocument, pptx: presentation, xlsx: spreadsheet };
let session;

/** The file's bytes; the app refuses it past the limit, with the message the page shows. */
async function readDocument(signal) {
  const response = await fetch("/document", { signal });
  if (!response.ok)
    throw new Error(
      (await response.text()) || "The file could not be loaded. Close it and try again.",
    );
  const bytes = await response.arrayBuffer();
  if (bytes.byteLength > OFFICE_PREVIEW_MAX_BYTES)
    throw new Error("Preview limited to files under 50 MB.");
  return bytes;
}

function noPreview(message) {
  return h(
    "div",
    { role: "alert", class: "no-preview" },
    h("p", { class: "no-preview-title" }, "No preview for this file"),
    h("p", { class: "no-preview-detail" }, `${message} Save it or open it in another app.`),
  );
}

function open() {
  session?.abort();
  const controller = new AbortController();
  session = controller;
  const { signal } = controller;
  root.replaceChildren(loading());
  // A failure replaces the viewer with its notice and stops its workers.
  const fail = (cause) => {
    if (signal.aborted) return;
    root.replaceChildren(noPreview(messageOf(cause, "Could not render this Office document.")));
    controller.abort();
  };
  const worker = workers.prepare();
  const timer = setTimeout(
    () => fail(new Error("This document took too long to preview. Open it in another app.")),
    PREPARE_TIMEOUT_MS,
  );
  signal.addEventListener(
    "abort",
    () => {
      clearTimeout(timer);
      worker.terminate();
    },
    { once: true },
  );
  worker.addEventListener("error", () => fail(new Error("Could not load the Office viewer.")));
  worker.addEventListener("messageerror", () =>
    fail(new Error("Could not read the Office viewer response.")),
  );
  worker.addEventListener("message", (event) => {
    if (signal.aborted) return;
    clearTimeout(timer);
    if (event.data.type === "error") fail(new Error(event.data.message));
    else if ("bytes" in event.data) {
      worker.terminate();
      mount(event.data, signal, fail);
    }
  });
  readDocument(signal)
    .then((bytes) => {
      if (!signal.aborted) worker.postMessage({ type: "open", kind: state.kind, bytes }, [bytes]);
    })
    .catch(fail);
}

function mount(prepared, signal, fail) {
  const details = h("span", { class: "details", "aria-live": "polite" });
  const subheader = h("div", { class: "subheader", hidden: true }, details);
  const onDetails = (text) => {
    details.textContent = text ? `${text}${state.kind !== "xlsx" ? " · Fit to width" : ""}` : "";
    subheader.hidden = !text;
  };
  const viewer = viewers[state.kind](prepared.bytes, { onDetails, onError: fail, signal });
  root.replaceChildren(
    subheader,
    prepared.truncated
      ? notice("Preview limited. Save it or open it in another app to read the full document.")
      : "",
    viewer,
  );
}

window.t3Office = {
  /** A new revision of the file opens again, as the web remounts its viewer for a new source. */
  reload: open,
  theme(css) {
    if (theme.textContent !== css) theme.textContent = css;
  },
};

document.title = state.title;
open();
