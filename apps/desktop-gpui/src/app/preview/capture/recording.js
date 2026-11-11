(() => {
  if (window !== window.top || window.__t3Recording) return;
  // `RecordingCursor.ts` and `recordingCompositor.ts`: the screen capture has no native cursor and
  // no compositor, so the cursor and the key and mouse press overlays are drawn in the page, in the
  // top layer as manual popovers so dialogs and fullscreen elements stay under them.
  const RESET =
    "all:initial;margin:0;padding:0;border:0;background:transparent;overflow:visible;inset:auto;animation:none;transition:none;";
  const LABELS = {
    Enter: "↵",
    Tab: "⇥",
    Backspace: "⌫",
    Delete: "⌦",
    Escape: "Esc",
    ArrowUp: "↑",
    ArrowDown: "↓",
    ArrowLeft: "←",
    ArrowRight: "→",
    " ": "Space",
    Space: "Space",
  };
  const MODIFIER_KEYS = ["Control", "Alt", "Shift", "Meta"];
  const keyLabel = (input, mac) => {
    if (["Dead", "Process", "Unidentified", ""].includes(input.key)) return null;
    const parts = [
      input.ctrlKey || input.key === "Control" ? (mac ? "⌃" : "Ctrl") : null,
      input.altKey || input.key === "Alt" ? (mac ? "⌥" : "Alt") : null,
      input.shiftKey || input.key === "Shift" ? (mac ? "⇧" : "Shift") : null,
      input.metaKey || input.key === "Meta" ? (mac ? "⌘" : "Win") : null,
    ].filter((part) => part !== null);
    if (!MODIFIER_KEYS.includes(input.key)) {
      parts.push(
        LABELS[input.key] ?? (input.key.length === 1 ? input.key.toUpperCase() : input.key),
      );
    }
    return parts.join(mac ? "" : " + ");
  };
  // Unknown iframe or closed-shadow focus is excluded because its field type cannot be checked.
  const sensitive = () => {
    let element = document.activeElement;
    while (element?.shadowRoot?.activeElement) element = element.shadowRoot.activeElement;
    return (
      element?.tagName === "IFRAME" ||
      element?.tagName.includes("-") === true ||
      element?.getAttribute("type")?.toLowerCase() === "password"
    );
  };
  let active = null;
  const stop = () => {
    active?.();
    active = null;
  };
  const start = (options) => {
    stop();
    const layer = (css, html) => {
      const element = document.createElement("div");
      element.setAttribute("aria-hidden", "true");
      element.setAttribute("data-t3code-recording", "");
      element.popover = "manual";
      element.style.cssText =
        RESET +
        "position:fixed;left:0;top:0;pointer-events:none;z-index:2147483647;display:none;" +
        css;
      if (html) element.innerHTML = html;
      return element;
    };
    const style = document.createElement("style");
    style.textContent =
      "html, html * { cursor: none !important; } [data-t3code-recording]::backdrop { display: none !important; }";
    const cursor = layer(
      "width:16px;height:24px;",
      '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="24" viewBox="0 0 16 24"><path d="M1 1v18l4-4 4 8 3-1.5-4-8H15Z" fill="black" stroke="white" stroke-width="1.5" stroke-linejoin="round"/></svg>',
    );
    const ring = layer("width:40px;height:40px;margin:-20px 0 0 -20px;");
    const circle = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    circle.setAttribute("width", "40");
    circle.setAttribute("height", "40");
    circle.innerHTML = '<circle cx="20" cy="20" r="19" stroke-width="2" fill-opacity="0.15"/>';
    circle.firstElementChild.setAttribute("fill", options.primary);
    circle.firstElementChild.setAttribute("stroke", options.primary);
    ring.append(circle);
    const badge = layer(
      "left:50%;top:auto;bottom:24px;transform:translateX(-50%);box-sizing:border-box;max-width:calc(100vw - 32px);height:54px;padding:0 18px;border-radius:14px;background:rgba(32,32,34,.86);color:white;font:500 26px system-ui,sans-serif;line-height:54px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;",
    );
    const layers = [ring, badge, cursor];
    document.documentElement.append(style, ...layers);
    const raise = () => {
      for (const element of layers) {
        if (!element.isConnected || typeof element.showPopover !== "function") continue;
        if (element.matches(":popover-open")) element.hidePopover();
        element.showPopover();
      }
    };
    raise();
    const dialogObserver = new MutationObserver(raise);
    dialogObserver.observe(document, {
      subtree: true,
      attributes: true,
      attributeFilter: ["open"],
    });
    const toggle = (event) => {
      if (layers.includes(event.target)) return;
      if (event.newState === "open") raise();
    };
    document.addEventListener("toggle", toggle, true);
    document.addEventListener("fullscreenchange", raise);

    let ringHeld = false;
    let ringTimer;
    let keyTimer;
    let lastKeyLabel = null;
    const place = (element, point, extra = "") => {
      element.style.transform = `translate(${point.x}px, ${point.y}px)${extra}`;
      element.style.display = "block";
    };
    const clearRing = () => {
      window.clearTimeout(ringTimer);
      ringHeld = false;
      ring.style.display = "none";
    };
    const pressRing = (point) => {
      clearRing();
      ringHeld = true;
      ring.style.transition = "none";
      ring.style.opacity = "0.9";
      place(ring, point);
    };
    const releaseRing = (point) => {
      ringHeld = false;
      place(ring, point);
      // Released rings grow by half and fade out over 600ms, as the compositor draws them.
      void ring.offsetWidth;
      ring.style.transition = "transform 600ms linear, opacity 600ms linear";
      ring.style.transform = `translate(${point.x}px, ${point.y}px) scale(1.5)`;
      ring.style.opacity = "0";
      ringTimer = window.setTimeout(clearRing, 600);
    };
    const showKey = (label, held) => {
      window.clearTimeout(keyTimer);
      if (!label) {
        badge.style.display = "none";
        return;
      }
      badge.textContent = label;
      badge.style.display = "block";
      if (!held) keyTimer = window.setTimeout(() => (badge.style.display = "none"), 900);
    };
    const pointerMove = (event) => {
      if (event.pointerType === "touch") return;
      const point = { x: event.clientX, y: event.clientY };
      place(cursor, point);
      if (options.mouse && ringHeld) place(ring, point);
    };
    const pointerDown = (event) => {
      if (event.pointerType === "touch") return;
      const point = { x: event.clientX, y: event.clientY };
      place(cursor, point);
      if (options.mouse) pressRing(point);
    };
    const pointerUp = (event) => {
      if (event.pointerType === "touch" || !options.mouse || !ringHeld) return;
      releaseRing({ x: event.clientX, y: event.clientY });
    };
    const keyDown = (event) => {
      if (event.isComposing || event.repeat || !options.keys) return;
      lastKeyLabel = sensitive() ? null : keyLabel(event, options.mac);
      showKey(lastKeyLabel, true);
    };
    const keyUp = () => {
      if (!options.keys) return;
      showKey(sensitive() ? null : lastKeyLabel, false);
    };
    const hide = () => {
      cursor.style.display = "none";
      clearRing();
      showKey(null, false);
    };
    const leave = (event) => {
      if (event.relatedTarget === null) hide();
    };
    const listeners = [
      ["pointermove", pointerMove, true],
      ["pointerdown", pointerDown, true],
      ["pointerup", pointerUp, true],
      ["pointercancel", pointerUp, true],
      ["keydown", keyDown, true],
      ["keyup", keyUp, true],
      ["pointerout", leave, true],
      ["blur", hide, false],
    ];
    for (const [type, listener, capture] of listeners)
      window.addEventListener(type, listener, capture);
    active = () => {
      for (const [type, listener, capture] of listeners)
        window.removeEventListener(type, listener, capture);
      dialogObserver.disconnect();
      document.removeEventListener("toggle", toggle, true);
      document.removeEventListener("fullscreenchange", raise);
      window.clearTimeout(ringTimer);
      window.clearTimeout(keyTimer);
      for (const element of layers) element.remove();
      style.remove();
    };
  };
  Object.defineProperty(window, "__t3Recording", { value: Object.freeze({ start, stop }) });
})();
