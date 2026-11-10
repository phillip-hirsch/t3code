(() => {
  if (window !== window.top || window.__t3Annotation || !window.ipc) return;
  // `PickPreload.ts` without its voice input. The app starts a session with a nonce and hears back
  // over wry's IPC; the page's own scripts can't reach the closed shadow root, and the posting
  // functions are taken before they run.
  const postMessage = window.ipc.postMessage.bind(window.ipc);
  const stringify = JSON.stringify;
  const OVERLAY_ATTRIBUTE = "data-t3code-annotation-ui";
  const INPUT_LAYER_ATTRIBUTE = "data-t3code-annotation-input";
  const Z_INDEX_OVERLAY = 2147483646;
  const PRIMARY = "var(--t3-primary)";
  const PRIMARY_FILL = "color-mix(in srgb, var(--t3-primary) 10%, transparent)";
  const MAX_MARQUEE_ELEMENTS = 20;
  const HTML_PREVIEW_MAX_CHARS = 500;
  const CONTENT_LAYER_Z_INDEX = 1;
  const CHROME_LAYER_Z_INDEX = 10;
  // Page dialogs (Radix, react-remove-scroll) dismiss, refocus or block scrolling on document events
  // that shadow retargeting attributes to the overlay host.
  const PAGE_ISOLATED_EVENTS = [
    "pointerdown",
    "focusin",
    "focusout",
    "wheel",
    "touchstart",
    "touchmove",
  ];
  // The generated Tailwind of `Annotation.css`, for the classes the overlay uses.
  const STYLES = `
:host { all: initial; }
* { box-sizing: border-box; border-color: var(--t3-border); }
button, input, select, textarea { font: inherit; margin: 0; }
button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible {
  outline: 2px solid color-mix(in srgb, var(--t3-ring) 72%, transparent); outline-offset: -2px; }
[hidden] { display: none !important; }
input[type=number]::-webkit-inner-spin-button, input[type=number]::-webkit-outer-spin-button { appearance: none; margin: 0; }
.root { position: fixed; inset: 0; font-family: var(--t3-font-sans); color: var(--t3-foreground); pointer-events: none; }
.btn { display: inline-flex; height: 28px; cursor: pointer; align-items: center; justify-content: center;
  border-radius: calc(var(--t3-radius) - 2px); border: 1px solid transparent; background: transparent; padding: 0 8px;
  font-family: var(--t3-font-sans); font-size: 12px; line-height: 16px; font-weight: 500; color: var(--t3-foreground); outline: none; }
.btn:hover { background: var(--t3-accent); }
.btn:disabled { pointer-events: none; opacity: 0.6; }
.tool { height: 32px; padding: 0 10px; font-size: 14px; line-height: 20px; }
.tool[data-active] { background: color-mix(in srgb, var(--t3-primary) 10%, transparent); color: var(--t3-primary); }
.toolbar { pointer-events: auto; position: fixed; top: 10px; left: 50%; transform: translateX(-50%); display: flex; gap: 2px;
  border-radius: var(--t3-radius); border: 1px solid var(--t3-border); background: color-mix(in srgb, var(--t3-popover) 95%, transparent);
  padding: 4px; color: var(--t3-popover-foreground); backdrop-filter: blur(24px); z-index: ${CHROME_LAYER_Z_INDEX};
  box-shadow: 0 10px 15px -3px rgb(0 0 0 / 0.1), 0 4px 6px -4px rgb(0 0 0 / 0.1); }
.editor { pointer-events: auto; position: fixed; display: none; max-height: calc(100vh - 16px); width: min(360px, calc(100vw - 16px));
  flex-direction: column; overflow: hidden; border-radius: calc(var(--t3-radius) + 4px); border: 1px solid var(--t3-border);
  background: color-mix(in srgb, var(--t3-popover) 96%, transparent); color: var(--t3-popover-foreground);
  backdrop-filter: blur(24px); z-index: ${CHROME_LAYER_Z_INDEX}; box-shadow: 0 25px 50px -12px rgb(0 0 0 / 0.25); }
.composer { display: flex; align-items: flex-start; gap: 8px; padding: 8px; }
.adjust { height: 32px; width: 32px; flex-shrink: 0; background: var(--t3-muted); padding: 0; color: var(--t3-muted-foreground); }
.adjust:hover { background: var(--t3-accent); color: var(--t3-accent-foreground); }
.comment { min-height: 32px; max-height: 96px; min-width: 0; flex: 1; resize: none; overflow-y: hidden; border: 0;
  border-bottom: 1px solid transparent; background: transparent; padding: 6px 0; font-family: var(--t3-font-sans);
  font-size: 14px; line-height: 20px; color: var(--t3-foreground); outline: none; }
.comment::placeholder { color: var(--t3-muted-foreground); }
.comment:focus, .comment:focus-visible { border-bottom-color: var(--t3-primary); outline: none; }
.drag { display: none; height: 32px; width: 24px; flex-shrink: 0; cursor: grab; user-select: none; border: 0; background: transparent;
  padding: 0; font-family: var(--t3-font-sans); font-size: 18px; font-weight: 700; line-height: 20px; color: var(--t3-muted-foreground); }
.submit { height: 32px; flex-shrink: 0; border-color: var(--t3-primary); background: var(--t3-primary); padding: 0 12px;
  color: var(--t3-primary-foreground); box-shadow: 0 1px 3px 0 rgb(0 0 0 / 0.1), 0 1px 2px -1px rgb(0 0 0 / 0.1); }
.submit:hover { background: color-mix(in srgb, var(--t3-primary) 90%, transparent); }
.styles { display: none; max-height: min(176px, calc(100vh - 180px)); overflow: auto; border-top: 1px solid var(--t3-border);
  background: color-mix(in srgb, var(--t3-muted) 40%, transparent); padding: 0 12px; }
.section { display: grid; gap: 4px; border-top: 1px solid var(--t3-border); padding: 8px 0; }
.field { display: grid; min-height: 28px; grid-template-columns: 82px minmax(0, 1fr); align-items: center; gap: 8px;
  font-family: var(--t3-font-sans); font-size: 12px; line-height: 16px; font-weight: 500; color: var(--t3-muted-foreground); }
.dimension-labels { display: grid; gap: 8px; font-family: var(--t3-font-sans); font-size: 12px; line-height: 16px;
  font-weight: 500; color: var(--t3-muted-foreground); }
.control { height: 28px; min-width: 0; width: 100%; appearance: none; border-radius: calc(var(--t3-radius) - 2px);
  border: 1px solid var(--t3-input); background: var(--t3-background); padding: 0 8px; font-family: var(--t3-font-mono);
  font-size: 12px; color: var(--t3-foreground); box-shadow: 0 1px 2px 0 rgb(0 0 0 / 0.05); outline: none; }
input[type=range].control { padding: 0; border: 0; box-shadow: none; background: transparent; appearance: auto; }
.unit { pointer-events: none; position: absolute; top: 50%; right: 8px; transform: translateY(-50%);
  font-family: var(--t3-font-mono); font-size: 12px; color: var(--t3-muted-foreground); }
.swatch { display: grid; height: 28px; grid-template-columns: 22px minmax(0, 1fr); align-items: center; gap: 4px;
  border-radius: calc(var(--t3-radius) - 2px); border: 1px solid var(--t3-input); background: var(--t3-background); padding: 0 4px;
  box-shadow: 0 1px 2px 0 rgb(0 0 0 / 0.05); }
.swatch-text { min-width: 0; width: 100%; border: 0; background: transparent; font-family: var(--t3-font-mono); font-size: 12px;
  color: var(--t3-foreground); outline: none; }
.lock[aria-pressed=true] { background: color-mix(in srgb, var(--t3-primary) 10%, transparent); color: var(--t3-primary); }
.lock[aria-pressed=false] { background: var(--t3-muted); color: var(--t3-muted-foreground); }
.label { position: fixed; max-width: 280px; overflow: hidden; border-radius: calc(var(--t3-radius) - 2px); background: var(--t3-primary);
  padding: 4px 8px; font-family: var(--t3-font-sans); font-size: 12px; line-height: 16px; font-weight: 600;
  color: var(--t3-primary-foreground); box-shadow: 0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1);
  pointer-events: none; white-space: nowrap; text-overflow: ellipsis; z-index: ${CONTENT_LAYER_Z_INDEX}; }
`;
  // Computed properties an element's context reports when they differ from its tag's defaults.
  const STYLE_PROPERTIES = [
    "display",
    "position",
    "top",
    "right",
    "bottom",
    "left",
    "z-index",
    "flex-direction",
    "flex-wrap",
    "align-items",
    "justify-content",
    "gap",
    "grid-template-columns",
    "width",
    "height",
    "min-width",
    "min-height",
    "max-width",
    "max-height",
    "margin",
    "padding",
    "overflow",
    "color",
    "background-color",
    "background-image",
    "border",
    "border-radius",
    "box-shadow",
    "opacity",
    "font-family",
    "font-size",
    "font-weight",
    "line-height",
    "text-align",
    "text-decoration",
    "text-transform",
    "white-space",
    "cursor",
    "transform",
  ];

  /** Per-load nonces avoid draft ID collisions (`AnnotationIds.ts`). */
  const pageLoad = Array.from(crypto.getRandomValues(new Uint32Array(2)), (part) =>
    part.toString(36),
  ).join("");
  let sequence = 0;
  const nextId = (prefix) => {
    sequence += 1;
    return `${prefix}_${pageLoad}_${sequence.toString(36)}`;
  };

  /** `resolveAnnotationSubmission` (`AnnotationKeyboard.ts`). */
  const resolveSubmission = (event, canSend) => {
    if (event.key !== "Enter" || event.shiftKey || event.isComposing) return null;
    return event.metaKey || event.ctrlKey ? (canSend ? "send" : null) : "attach";
  };

  const rectFromDomRect = (rect) => ({
    x: rect.left,
    y: rect.top,
    width: rect.width,
    height: rect.height,
  });
  const normalizeRect = (startX, startY, endX, endY) => ({
    x: Math.min(startX, endX),
    y: Math.min(startY, endY),
    width: Math.abs(endX - startX),
    height: Math.abs(endY - startY),
  });
  const isUsableRect = (rect) => rect.width >= 3 && rect.height >= 3;
  const unionRects = (rects, padding = 20) => {
    if (rects.length === 0) return null;
    const left = Math.min(...rects.map((rect) => rect.x));
    const top = Math.min(...rects.map((rect) => rect.y));
    const right = Math.max(...rects.map((rect) => rect.x + rect.width));
    const bottom = Math.max(...rects.map((rect) => rect.y + rect.height));
    // The app clips to the captured viewport; clipping here would lose offscreen bounds.
    return {
      x: left - padding,
      y: top - padding,
      width: right - left + padding * 2,
      height: bottom - top + padding * 2,
    };
  };
  const isAnnotationNode = (element) =>
    element instanceof Element && element.closest(`[${OVERLAY_ATTRIBUTE}]`) !== null;
  const pickFromPoint = (clientX, clientY) => {
    for (const candidate of document.elementsFromPoint(clientX, clientY)) {
      if (!(candidate instanceof Element)) continue;
      if (isAnnotationNode(candidate) || candidate.hasAttribute(INPUT_LAYER_ATTRIBUTE)) continue;
      if (candidate === document.documentElement || candidate === document.body) continue;
      return candidate;
    }
    return null;
  };
  const describeRawElement = (element) => {
    const tag = element.tagName.toLowerCase();
    const id = element.id ? `#${element.id}` : "";
    const classes =
      element instanceof HTMLElement && typeof element.className === "string"
        ? element.className
            .trim()
            .split(/\s+/)
            .filter(Boolean)
            .slice(0, 2)
            .map((name) => `.${name}`)
            .join("")
        : "";
    return `${tag}${id}${classes}`;
  };
  const createBox = (color, fill) => {
    const node = document.createElement("div");
    node.setAttribute(OVERLAY_ATTRIBUTE, "");
    node.style.cssText = [
      "position:fixed",
      "pointer-events:none",
      `border:2px solid ${color}`,
      `background:${fill}`,
      "border-radius:3px",
      "box-sizing:border-box",
      "display:none",
      "left:0",
      "top:0",
      `z-index:${CONTENT_LAYER_Z_INDEX}`,
    ].join(";");
    return node;
  };
  const positionBox = (node, rect) => {
    if (rect.width <= 0 || rect.height <= 0) {
      node.style.display = "none";
      return;
    }
    node.style.display = "block";
    node.style.transform = `translate(${rect.x}px, ${rect.y}px)`;
    node.style.width = `${rect.width}px`;
    node.style.height = `${rect.height}px`;
  };
  const visibleElementRect = (element) => {
    const rect = element.getBoundingClientRect();
    let left = Math.max(0, rect.left);
    let top = Math.max(0, rect.top);
    let right = Math.min(document.documentElement.clientWidth, rect.right);
    let bottom = Math.min(document.documentElement.clientHeight, rect.bottom);
    if (getComputedStyle(element).position !== "fixed") {
      for (let ancestor = element.parentElement; ancestor; ancestor = ancestor.parentElement) {
        if (ancestor === document.body || ancestor === document.documentElement) break;
        const style = getComputedStyle(ancestor);
        const bounds = ancestor.getBoundingClientRect();
        const clipLeft = bounds.left + ancestor.clientLeft;
        const clipTop = bounds.top + ancestor.clientTop;
        if (style.overflowX !== "visible") {
          left = Math.max(left, clipLeft);
          right = Math.min(right, clipLeft + ancestor.clientWidth);
        }
        if (style.overflowY !== "visible") {
          top = Math.max(top, clipTop);
          bottom = Math.min(bottom, clipTop + ancestor.clientHeight);
        }
      }
    }
    return { x: left, y: top, width: Math.max(0, right - left), height: Math.max(0, bottom - top) };
  };
  const createLabel = () => {
    const label = document.createElement("div");
    label.setAttribute(OVERLAY_ATTRIBUTE, "");
    label.className = "label";
    label.style.cssText = "left:0;top:0;display:none";
    return label;
  };
  const updateSelectedVisual = (target) => {
    if (!target.element.isConnected) {
      target.outline.style.display = "none";
      target.label.style.display = "none";
      return;
    }
    const rect = visibleElementRect(target.element);
    positionBox(target.outline, rect);
    if (rect.width === 0 || rect.height === 0) {
      target.label.style.display = "none";
      return;
    }
    target.label.textContent = describeRawElement(target.element);
    target.label.style.display = "block";
    target.label.style.transform = `translate(${Math.max(4, rect.x)}px, ${Math.max(4, rect.y - 22)}px)`;
  };

  // `getElementContext` (react-grab) is not bundled here: the selector, React component and
  // non-default styles come from these smaller lookups, and anything they miss stays null.
  const cssEscape = (value) =>
    typeof CSS !== "undefined" && typeof CSS.escape === "function"
      ? CSS.escape(value)
      : value.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
  const isUnique = (element, selector) => {
    try {
      const found = document.querySelectorAll(selector);
      return found.length === 1 && found[0] === element;
    } catch {
      return false;
    }
  };
  const SELECTOR_ATTRIBUTES = [
    "data-testid",
    "data-test-id",
    "data-test",
    "data-cy",
    "data-qa",
    "aria-label",
    "role",
    "name",
    "title",
    "alt",
  ];
  const elementSelector = (element) => {
    if (element instanceof HTMLElement && element.id) {
      const byId = `#${cssEscape(element.id)}`;
      if (isUnique(element, byId)) return byId;
    }
    for (const attribute of SELECTOR_ATTRIBUTES) {
      const value = element.getAttribute(attribute);
      if (!value || value.length > 120) continue;
      const byAttribute = `[${attribute}=${stringify(value)}]`;
      if (isUnique(element, byAttribute)) return byAttribute;
      const tagged = `${element.tagName.toLowerCase()}${byAttribute}`;
      if (isUnique(element, tagged)) return tagged;
    }
    const parts = [];
    const body = document.body ?? document.documentElement;
    for (let node = element; node;) {
      if (node instanceof HTMLElement && node.id) {
        parts.unshift(`#${cssEscape(node.id)}`);
        break;
      }
      const parent = node.parentElement;
      if (!parent) {
        parts.unshift(node.tagName.toLowerCase());
        break;
      }
      parts.unshift(
        `${node.tagName.toLowerCase()}:nth-child(${Array.from(parent.children).indexOf(node) + 1})`,
      );
      if (parent === body) {
        parts.unshift(body.tagName.toLowerCase());
        break;
      }
      node = parent;
    }
    return parts.join(" > ");
  };
  const reactFiber = (element) => {
    for (const key of Object.keys(element)) {
      if (key.startsWith("__reactFiber$") || key.startsWith("__reactInternalInstance$"))
        return element[key];
    }
    return null;
  };
  const componentOf = (element) => {
    for (
      let fiber = reactFiber(element), depth = 0;
      fiber && depth < 50;
      fiber = fiber.return, depth += 1
    ) {
      const type = fiber.type;
      const component =
        typeof type === "function"
          ? type
          : type && typeof type === "object"
            ? (type.render ?? type.type)
            : null;
      const name = component && (component.displayName || component.name);
      if (typeof name === "string" && name.length > 0) {
        const source = fiber._debugSource;
        return {
          componentName: name,
          source:
            source && typeof source.fileName === "string"
              ? {
                  functionName: name,
                  fileName: source.fileName,
                  lineNumber: Number.isInteger(source.lineNumber) ? source.lineNumber : null,
                  columnNumber: Number.isInteger(source.columnNumber) ? source.columnNumber : null,
                }
              : null,
        };
      }
    }
    return { componentName: null, source: null };
  };
  let baselineFrame = null;
  const baselines = new Map();
  const baselineStyles = (tag) => {
    const cached = baselines.get(tag);
    if (cached) return cached;
    const styles = new Map();
    try {
      if (!baselineFrame) {
        baselineFrame = document.createElement("iframe");
        baselineFrame.setAttribute(OVERLAY_ATTRIBUTE, "");
        baselineFrame.style.cssText =
          "position:fixed;left:-9999px;width:0;height:0;border:none;visibility:hidden";
        document.documentElement.appendChild(baselineFrame);
      }
      const frameDocument = baselineFrame.contentDocument;
      const probe = frameDocument.createElement(tag);
      frameDocument.body.appendChild(probe);
      const computed = baselineFrame.contentWindow.getComputedStyle(probe);
      for (const property of STYLE_PROPERTIES)
        styles.set(property, computed.getPropertyValue(property));
      probe.remove();
    } catch {}
    baselines.set(tag, styles);
    return styles;
  };
  const elementStyles = (element) => {
    const baseline = baselineStyles(element.tagName.toLowerCase());
    const computed = getComputedStyle(element);
    const lines = [];
    for (const property of STYLE_PROPERTIES) {
      const value = computed.getPropertyValue(property);
      if (value && value !== baseline.get(property)) lines.push(`${property}: ${value};`);
    }
    const className = element.getAttribute("class")?.trim();
    const styles = lines.join("\n");
    return className
      ? styles
        ? `className: ${className}\n\n${styles}`
        : `className: ${className}`
      : styles;
  };
  /** Keeps the tag, HTML and crop rect when a lookup fails. */
  const captureElement = (element) => {
    const base = {
      pageUrl: location.href,
      pageTitle: document.title?.trim() || null,
      tagName: element.tagName.toLowerCase(),
      htmlPreview: element.outerHTML.slice(0, HTML_PREVIEW_MAX_CHARS),
      pickedAt: new Date().toISOString(),
    };
    try {
      const { componentName, source } = componentOf(element);
      return {
        ...base,
        selector: elementSelector(element),
        componentName,
        source,
        stack: source ? [source] : [],
        styles: elementStyles(element),
      };
    } catch {
      return { ...base, selector: null, componentName: null, source: null, stack: [], styles: "" };
    }
  };

  const createButton = (label, title, extra = "") => {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = label;
    button.title = title;
    button.className = `btn ${extra}`.trim();
    return button;
  };
  const createUnitControl = (input) => {
    const wrapper = document.createElement("div");
    wrapper.style.cssText = "position:relative;min-width:0";
    const unit = document.createElement("span");
    unit.textContent = input.dataset.unit ?? "";
    unit.className = "unit";
    wrapper.append(input, unit);
    return wrapper;
  };
  const styleControl = (input) => {
    input.setAttribute("aria-label", input.getAttribute("aria-label") ?? "Style value");
    input.className = "control";
  };
  const createField = (labelText, input) => {
    const label = document.createElement("label");
    label.className = "field";
    const text = document.createElement("span");
    text.textContent = labelText;
    styleControl(input);
    label.append(
      text,
      input instanceof HTMLInputElement && input.dataset.unit ? createUnitControl(input) : input,
    );
    return label;
  };
  const createStyleSection = () => {
    const section = document.createElement("section");
    section.className = "section";
    return section;
  };
  const createUnitInput = (unit, placeholder = "0") => {
    const input = document.createElement("input");
    input.type = "number";
    input.placeholder = placeholder;
    input.style.paddingRight = "30px";
    input.dataset.unit = unit;
    return input;
  };
  const pathFromPoints = (points) => {
    if (points.length === 0) return "";
    if (points.length === 1) return `M ${points[0].x} ${points[0].y} l 0.01 0.01`;
    let path = `M ${points[0].x} ${points[0].y}`;
    for (let index = 1; index < points.length - 1; index += 1) {
      const current = points[index];
      const next = points[index + 1];
      path += ` Q ${current.x} ${current.y} ${(current.x + next.x) / 2} ${(current.y + next.y) / 2}`;
    }
    const last = points[points.length - 1];
    return `${path} L ${last.x} ${last.y}`;
  };
  const strokeBounds = (points, width) => {
    const xs = points.map((point) => point.x);
    const ys = points.map((point) => point.y);
    const padding = width + 3;
    const left = Math.min(...xs) - padding;
    const top = Math.min(...ys) - padding;
    const right = Math.max(...xs) + padding;
    const bottom = Math.max(...ys) + padding;
    return { x: left, y: top, width: right - left, height: bottom - top };
  };
  const THEME_VARIABLES = {
    radius: "--t3-radius",
    background: "--t3-background",
    foreground: "--t3-foreground",
    popover: "--t3-popover",
    popoverForeground: "--t3-popover-foreground",
    primary: "--t3-primary",
    primaryForeground: "--t3-primary-foreground",
    destructive: "--t3-destructive",
    muted: "--t3-muted",
    mutedForeground: "--t3-muted-foreground",
    accent: "--t3-accent",
    accentForeground: "--t3-accent-foreground",
    border: "--t3-border",
    input: "--t3-input",
    ring: "--t3-ring",
    fontSans: "--t3-font-sans",
    fontMono: "--t3-font-mono",
  };

  let active = null;

  const startAnnotation = ({ session, sendEnabled: initialSendEnabled, theme }) => {
    active?.teardown(false);
    let sendEnabled = initialSendEnabled === true;
    const post = (message) => postMessage(stringify({ t3Annotation: { session, ...message } }));
    let finished = false;
    const host = document.createElement("div");
    host.setAttribute(OVERLAY_ATTRIBUTE, "");
    host.setAttribute("dir", "ltr");
    host.style.cssText = `position:fixed;inset:0;z-index:${Z_INDEX_OVERLAY};pointer-events:none`;
    if (theme && typeof theme === "object") {
      if (theme.colorScheme === "dark" || theme.colorScheme === "light")
        host.style.colorScheme = theme.colorScheme;
      for (const [key, variable] of Object.entries(THEME_VARIABLES)) {
        if (typeof theme[key] === "string") host.style.setProperty(variable, theme[key]);
      }
    }
    const shadowRoot = host.attachShadow({ mode: "closed" });
    const themeStyle = document.createElement("style");
    themeStyle.textContent = STYLES;
    shadowRoot.appendChild(themeStyle);

    const root = document.createElement("div");
    root.setAttribute(OVERLAY_ATTRIBUTE, "");
    root.setAttribute("dir", "ltr");
    root.className = "root";
    const cursorStyle = document.createElement("style");
    cursorStyle.setAttribute(OVERLAY_ATTRIBUTE, "");
    cursorStyle.textContent = `html[data-t3code-annotation-tool] body, html[data-t3code-annotation-tool] body * { cursor: crosshair !important; } [${OVERLAY_ATTRIBUTE}], [${OVERLAY_ATTRIBUTE}] * { cursor: default !important; }`;
    document.documentElement.appendChild(cursorStyle);
    shadowRoot.appendChild(root);

    const hoverOutline = createBox(PRIMARY, PRIMARY_FILL);
    const marqueeBox = createBox(PRIMARY, PRIMARY_FILL);
    root.append(hoverOutline, marqueeBox);

    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute(OVERLAY_ATTRIBUTE, "");
    svg.setAttribute("width", "100%");
    svg.setAttribute("height", "100%");
    svg.setAttribute("viewBox", `0 0 ${window.innerWidth} ${window.innerHeight}`);
    svg.style.cssText = `position:fixed;inset:0;overflow:visible;pointer-events:none;z-index:${CONTENT_LAYER_Z_INDEX}`;
    root.appendChild(svg);

    const toolbar = document.createElement("div");
    toolbar.setAttribute(OVERLAY_ATTRIBUTE, "");
    toolbar.className = "toolbar";
    root.appendChild(toolbar);

    const editor = document.createElement("div");
    editor.setAttribute(OVERLAY_ATTRIBUTE, "");
    editor.className = "editor";
    root.appendChild(editor);

    const composerRow = document.createElement("div");
    composerRow.className = "composer";

    const adjust = createButton("", "Expand annotation editor", "adjust");
    adjust.setAttribute("aria-label", "Expand annotation editor");
    adjust.setAttribute("aria-expanded", "false");
    adjust.innerHTML =
      '<svg viewBox="0 0 20 20" width="15" height="15" aria-hidden="true"><path d="M4 5h12M4 10h12M4 15h12M7 3v4M13 8v4M9 13v4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>';
    composerRow.appendChild(adjust);

    const comment = document.createElement("textarea");
    comment.placeholder = "Describe the change…";
    comment.setAttribute("dir", "auto");
    comment.rows = 1;
    comment.className = "comment";
    composerRow.appendChild(comment);

    const dragHandle = document.createElement("button");
    dragHandle.type = "button";
    dragHandle.textContent = "⠿";
    dragHandle.title = "Drag annotation editor";
    dragHandle.className = "drag";
    composerRow.appendChild(dragHandle);

    const submit = createButton("Attach", "Attach annotation and screenshot (Enter)", "submit");
    const updateSendHint = () => {
      submit.title = sendEnabled
        ? "Attach annotation and screenshot (Enter). Send with Cmd/Ctrl+Enter."
        : "Attach annotation and screenshot (Enter)";
    };
    updateSendHint();
    composerRow.appendChild(submit);
    editor.appendChild(composerRow);

    const stylePanel = document.createElement("div");
    stylePanel.className = "styles";
    editor.appendChild(stylePanel);

    const selected = new Map();
    const regions = [];
    const strokes = [];
    const styleChanges = new Map();
    const toolButtons = new Map();
    // A PDF draws in the browser's own viewer, whose pointer and key input never reaches this
    // document: a transparent layer above it catches the pointer and takes focus. It has no
    // elements to select, so it starts on Region.
    const isPdf = document.contentType === "application/pdf";
    const inputLayer = isPdf ? document.createElement("div") : null;
    if (inputLayer) {
      inputLayer.setAttribute(INPUT_LAYER_ATTRIBUTE, "");
      inputLayer.tabIndex = -1;
      inputLayer.style.cssText = `position:fixed;inset:0;z-index:${Z_INDEX_OVERLAY - 1};cursor:crosshair;outline:none`;
    }
    let tool = isPdf ? "marquee" : "select";
    let dragStart = null;
    let activeStroke = null;
    let pendingCapture = false;
    let editorExpanded = false;
    let editorWasShown = false;
    let editorPosition = null;
    let editorDrag = null;
    let editorLayoutFrame = null;

    const resizeComment = () => {
      const maxHeight = 96;
      comment.style.height = "auto";
      comment.style.height = `${Math.min(comment.scrollHeight, maxHeight)}px`;
      comment.style.overflowY = comment.scrollHeight > maxHeight ? "auto" : "hidden";
      queueEditorLayout();
    };
    comment.addEventListener("input", resizeComment);

    const hasTargets = () => selected.size > 0 || regions.length > 0 || strokes.length > 0;
    const updateStatus = () => {
      const targets = hasTargets();
      editor.style.display = targets ? "flex" : "none";
      submit.disabled = !targets || pendingCapture;
      submit.style.opacity = targets ? "1" : "0.45";
      adjust.disabled = !targets;
      stylePanel.style.display = editorExpanded && selected.size > 0 ? "grid" : "none";
      queueEditorLayout();
      if (targets && !editorWasShown) {
        editorWasShown = true;
        window.setTimeout(() => comment.focus({ preventScroll: true }), 0);
      }
    };

    const refreshToolButtons = () => {
      for (const [candidate, button] of toolButtons)
        button.toggleAttribute("data-active", candidate === tool);
      if (tool !== "select") hoverOutline.style.display = "none";
      if (tool !== "marquee") marqueeBox.style.display = "none";
      document.documentElement.setAttribute("data-t3code-annotation-tool", tool);
    };

    const restoreTargetStyles = (target) => {
      if (!(target.element instanceof HTMLElement || target.element instanceof SVGElement)) return;
      for (const [property, baseline] of target.baselineStyles) {
        if (baseline) target.element.style.setProperty(property, baseline);
        else target.element.style.removeProperty(property);
      }
    };
    const removeSelected = (target) => {
      restoreTargetStyles(target);
      selected.delete(target.element);
      target.outline.remove();
      target.label.remove();
      for (const [key, change] of styleChanges) {
        if (change.targetId === target.id) styleChanges.delete(key);
      }
    };
    const addSelected = (element) => {
      if (selected.has(element)) return;
      const target = {
        id: nextId("element"),
        element,
        outline: createBox(PRIMARY, PRIMARY_FILL),
        label: createLabel(),
        baselineStyles: new Map(),
      };
      selected.set(element, target);
      root.append(target.outline, target.label);
      updateSelectedVisual(target);
      if (editorExpanded) {
        stylePanel.style.display = "grid";
        syncStyleControls();
      }
    };
    const toggleSelected = (element, additive) => {
      const existing = selected.get(element);
      if (existing) {
        removeSelected(existing);
      } else {
        if (!additive) for (const target of Array.from(selected.values())) removeSelected(target);
        addSelected(element);
      }
      updateStatus();
    };
    const setStyleForSelected = (property, value) => {
      for (const target of selected.values()) {
        if (!(target.element instanceof HTMLElement || target.element instanceof SVGElement))
          continue;
        if (!target.baselineStyles.has(property)) {
          target.baselineStyles.set(property, target.element.style.getPropertyValue(property));
        }
        const key = `${target.id}:${property}`;
        const previousValue =
          styleChanges.get(key)?.previousValue ??
          getComputedStyle(target.element).getPropertyValue(property).trim();
        target.element.style.setProperty(property, value, "important");
        styleChanges.set(key, {
          targetId: target.id,
          selector: null,
          property,
          previousValue,
          value,
        });
        updateSelectedVisual(target);
      }
    };

    const textSection = createStyleSection();
    const colorsSection = createStyleSection();
    const bordersSection = createStyleSection();
    const sizingSection = createStyleSection();
    stylePanel.append(textSection, colorsSection, bordersSection, sizingSection);

    const createSelect = (values) => {
      const select = document.createElement("select");
      for (const value of values) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = value;
        select.appendChild(option);
      }
      return select;
    };
    const fontFamily = createSelect(["inherit", "system-ui", "sans-serif", "serif", "monospace"]);
    fontFamily.addEventListener("change", () =>
      setStyleForSelected("font-family", fontFamily.value),
    );
    textSection.appendChild(createField("Font", fontFamily));

    const fontSize = createUnitInput("px", "16");
    fontSize.min = "1";
    fontSize.max = "300";
    fontSize.addEventListener("input", () => {
      if (fontSize.value) setStyleForSelected("font-size", `${fontSize.value}px`);
    });
    textSection.appendChild(createField("Font size", fontSize));

    const fontWeight = createSelect(["300", "400", "500", "600", "700", "800", "900"]);
    fontWeight.addEventListener("change", () =>
      setStyleForSelected("font-weight", fontWeight.value),
    );
    textSection.appendChild(createField("Font weight", fontWeight));

    const lineHeight = document.createElement("input");
    lineHeight.type = "text";
    lineHeight.placeholder = "normal / 1.4";
    lineHeight.addEventListener("change", () => {
      if (lineHeight.value.trim()) setStyleForSelected("line-height", lineHeight.value.trim());
    });
    textSection.appendChild(createField("Line height", lineHeight));

    const createColorRow = (labelText, property, section) => {
      const row = document.createElement("label");
      row.className = "field";
      const label = document.createElement("span");
      label.textContent = labelText;
      const control = document.createElement("div");
      control.className = "swatch";
      const color = document.createElement("input");
      color.type = "color";
      color.setAttribute("aria-label", labelText);
      color.style.cssText =
        "width:20px;height:20px;padding:0;border:0;border-radius:5px;overflow:hidden;background:transparent;cursor:pointer";
      const text = document.createElement("input");
      text.type = "text";
      text.setAttribute("aria-label", `${labelText} value`);
      text.className = "swatch-text";
      color.addEventListener("input", () => {
        text.value = color.value;
        setStyleForSelected(property, color.value);
      });
      text.addEventListener("change", () => {
        const value = text.value.trim();
        if (!value) return;
        setStyleForSelected(property, value);
        if (/^#[0-9a-f]{6}$/i.test(value)) color.value = value;
      });
      control.append(color, text);
      row.append(label, control);
      section.appendChild(row);
      return { row, color, text };
    };
    const textColor = createColorRow("Text color", "color", colorsSection);
    const backgroundColor = createColorRow("Background", "background-color", colorsSection);

    const opacity = document.createElement("input");
    opacity.type = "range";
    opacity.min = "0";
    opacity.max = "1";
    opacity.step = "0.05";
    opacity.value = "1";
    opacity.style.accentColor = PRIMARY;
    opacity.addEventListener("input", () => setStyleForSelected("opacity", opacity.value));
    colorsSection.appendChild(createField("Opacity", opacity));

    const radius = createUnitInput("px", "0");
    radius.min = "0";
    radius.max = "300";
    radius.addEventListener("input", () => {
      if (radius.value) setStyleForSelected("border-radius", `${radius.value}px`);
    });
    bordersSection.appendChild(createField("Radius", radius));

    const borderColor = createColorRow("Border color", "border-color", bordersSection);

    const borderWidth = createUnitInput("px", "0");
    borderWidth.min = "0";
    borderWidth.max = "100";
    borderWidth.addEventListener("input", () => {
      if (borderWidth.value) {
        setStyleForSelected("border-style", "solid");
        setStyleForSelected("border-width", `${borderWidth.value}px`);
      }
    });
    bordersSection.appendChild(createField("Border width", borderWidth));

    const dimensions = document.createElement("div");
    dimensions.style.cssText =
      "display:grid;grid-template-columns:82px minmax(0,1fr);gap:8px;align-items:center";
    const dimensionLabel = document.createElement("div");
    dimensionLabel.className = "dimension-labels";
    dimensionLabel.innerHTML = "<span>Width</span><span>Height</span>";
    const dimensionControls = document.createElement("div");
    dimensionControls.style.cssText = "position:relative;display:grid;gap:3px;padding-left:22px";
    const widthInput = createUnitInput("px", "auto");
    const heightInput = createUnitInput("px", "auto");
    styleControl(widthInput);
    styleControl(heightInput);
    const aspectLock = createButton("", "Lock aspect ratio", "lock");
    aspectLock.setAttribute("aria-pressed", "true");
    aspectLock.style.cssText =
      "position:absolute;left:0;top:50%;transform:translateY(-50%);width:18px;height:38px;padding:0";
    dimensionControls.append(
      createUnitControl(widthInput),
      createUnitControl(heightInput),
      aspectLock,
    );
    dimensions.append(dimensionLabel, dimensionControls);
    sizingSection.appendChild(dimensions);

    let aspectLocked = true;
    let aspectRatio = 1;
    const refreshAspectButton = () => {
      aspectLock.innerHTML = aspectLocked
        ? '<svg viewBox="0 0 20 20" width="14" height="14" aria-hidden="true"><path d="M8 6.5 9.5 5A3.5 3.5 0 0 1 14.5 10l-1.5 1.5M12 13.5 10.5 15A3.5 3.5 0 0 1 5.5 10L7 8.5M7.5 12.5l5-5" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/></svg>'
        : '<svg viewBox="0 0 20 20" width="14" height="14" aria-hidden="true"><path d="m6 6 8 8M8 6.5 9.5 5A3.5 3.5 0 0 1 14 9M12 13.5 10.5 15A3.5 3.5 0 0 1 6 11" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/></svg>';
      aspectLock.setAttribute("aria-pressed", String(aspectLocked));
    };
    aspectLock.addEventListener("click", () => {
      aspectLocked = !aspectLocked;
      refreshAspectButton();
    });
    widthInput.addEventListener("input", () => {
      const width = Number(widthInput.value);
      if (!Number.isFinite(width) || width <= 0) return;
      setStyleForSelected("width", `${width}px`);
      if (aspectLocked && aspectRatio > 0) {
        const height = Math.max(1, Math.round(width / aspectRatio));
        heightInput.value = String(height);
        setStyleForSelected("height", `${height}px`);
      }
    });
    heightInput.addEventListener("input", () => {
      const height = Number(heightInput.value);
      if (!Number.isFinite(height) || height <= 0) return;
      setStyleForSelected("height", `${height}px`);
      if (aspectLocked && aspectRatio > 0) {
        const width = Math.max(1, Math.round(height * aspectRatio));
        widthInput.value = String(width);
        setStyleForSelected("width", `${width}px`);
      }
    });
    refreshAspectButton();

    const addSpacingField = (label, property, placeholder) => {
      const input = document.createElement("input");
      input.type = "text";
      input.placeholder = placeholder;
      input.addEventListener("change", () => {
        if (input.value.trim()) setStyleForSelected(property, input.value.trim());
      });
      sizingSection.appendChild(createField(label, input));
      return input;
    };
    const padding = addSpacingField("Padding", "padding", "0 0 0 0");
    const margin = addSpacingField("Margin", "margin", "0 0 0 0");
    const gap = addSpacingField("Gap", "gap", "0px");

    const syncStyleControls = () => {
      const first = selected.values().next().value;
      if (!first) return;
      const computed = getComputedStyle(first.element);
      const rect = first.element.getBoundingClientRect();
      aspectRatio = rect.height > 0 ? rect.width / rect.height : 1;
      widthInput.value = String(Math.round(rect.width));
      heightInput.value = String(Math.round(rect.height));
      fontSize.value = String(Math.round(Number.parseFloat(computed.fontSize) || 16));
      fontWeight.value = /^[0-9]+$/.test(computed.fontWeight) ? computed.fontWeight : "400";
      lineHeight.value = computed.lineHeight;
      fontFamily.value = Array.from(fontFamily.options).some(
        (option) => option.value === computed.fontFamily,
      )
        ? computed.fontFamily
        : "inherit";
      textColor.text.value = computed.color;
      backgroundColor.text.value = computed.backgroundColor;
      borderColor.text.value = computed.borderColor;
      opacity.value = computed.opacity;
      radius.value = String(Math.round(Number.parseFloat(computed.borderRadius) || 0));
      borderWidth.value = String(Math.round(Number.parseFloat(computed.borderWidth) || 0));
      padding.value = computed.padding;
      margin.value = computed.margin;
      gap.value = computed.gap === "normal" ? "0px" : computed.gap;
    };

    const tools = [
      ["select", "Select", "Select elements (V)"],
      ["marquee", "Region", "Draw a region or marquee-select elements (R)"],
      ["draw", "Draw", "Draw freehand (D)"],
      ["erase", "Erase", "Remove an annotation target (E)"],
    ];
    for (const [candidate, label, title] of tools) {
      const button = createButton(label, title, "tool");
      button.addEventListener("click", () => {
        tool = candidate;
        refreshToolButtons();
      });
      toolButtons.set(candidate, button);
      toolbar.appendChild(button);
    }

    const clampEditorPosition = (left, top) => {
      const edge = 8;
      const rect = editor.getBoundingClientRect();
      return {
        left: Math.min(Math.max(edge, left), Math.max(edge, window.innerWidth - rect.width - edge)),
        top: Math.min(Math.max(edge, top), Math.max(edge, window.innerHeight - rect.height - edge)),
      };
    };
    const applyEditorPosition = (position) => {
      const clamped = clampEditorPosition(position.left, position.top);
      editor.style.left = `${clamped.left}px`;
      editor.style.top = `${clamped.top}px`;
      editor.style.right = "auto";
      editor.style.bottom = "auto";
      if (editorExpanded) editorPosition = clamped;
    };
    const getAnnotationBounds = () =>
      unionRects(
        [
          ...Array.from(selected.values(), (target) => visibleElementRect(target.element)).filter(
            (rect) => rect.width > 0 && rect.height > 0,
          ),
          ...regions.map((region) => region.rect),
          ...strokes.map((stroke) => stroke.bounds),
        ],
        0,
      );
    const positionCompactEditor = () => {
      const bounds = getAnnotationBounds();
      if (!bounds) return;
      const editorRect = editor.getBoundingClientRect();
      const spacing = 8;
      const candidates = [
        { left: bounds.x + bounds.width + spacing, top: bounds.y },
        { left: bounds.x - editorRect.width - spacing, top: bounds.y },
        {
          left: bounds.x + bounds.width - editorRect.width,
          top: bounds.y + bounds.height + spacing,
        },
        {
          left: bounds.x + bounds.width - editorRect.width,
          top: bounds.y - editorRect.height - spacing,
        },
      ];
      const overflow = (position) =>
        Math.max(0, -position.left) +
        Math.max(0, -position.top) +
        Math.max(0, position.left + editorRect.width - window.innerWidth) +
        Math.max(0, position.top + editorRect.height - window.innerHeight);
      applyEditorPosition(
        candidates.reduce((current, candidate) =>
          overflow(candidate) < overflow(current) ? candidate : current,
        ),
      );
    };
    function queueEditorLayout() {
      if (editorLayoutFrame !== null) window.cancelAnimationFrame(editorLayoutFrame);
      editorLayoutFrame = window.requestAnimationFrame(() => {
        editorLayoutFrame = null;
        if (editor.style.display === "none") return;
        if (editorExpanded && editorPosition) applyEditorPosition(editorPosition);
        else positionCompactEditor();
      });
    }

    adjust.addEventListener("click", () => {
      if (selected.size === 0) return;
      editorExpanded = !editorExpanded;
      if (editorExpanded) {
        const rect = editor.getBoundingClientRect();
        editorPosition = { left: rect.left, top: rect.top };
        syncStyleControls();
      } else {
        editorPosition = null;
      }
      stylePanel.style.display = editorExpanded ? "grid" : "none";
      dragHandle.style.display = editorExpanded ? "block" : "none";
      const label = editorExpanded ? "Collapse annotation editor" : "Expand annotation editor";
      adjust.setAttribute("aria-expanded", String(editorExpanded));
      adjust.title = label;
      adjust.setAttribute("aria-label", label);
      queueEditorLayout();
    });

    const onEditorPointerDown = (event) => {
      if (event.button !== 0 || !editorExpanded) return;
      const rect = editor.getBoundingClientRect();
      editorDrag = {
        pointerId: event.pointerId,
        offsetX: event.clientX - rect.left,
        offsetY: event.clientY - rect.top,
      };
      dragHandle.setPointerCapture(event.pointerId);
      dragHandle.style.cursor = "grabbing";
      event.preventDefault();
      event.stopPropagation();
    };
    const onEditorPointerMove = (event) => {
      if (!editorDrag || editorDrag.pointerId !== event.pointerId) return;
      applyEditorPosition({
        left: event.clientX - editorDrag.offsetX,
        top: event.clientY - editorDrag.offsetY,
      });
      event.preventDefault();
      event.stopPropagation();
    };
    const onEditorPointerUp = (event) => {
      if (!editorDrag || editorDrag.pointerId !== event.pointerId) return;
      editorDrag = null;
      dragHandle.style.cursor = "grab";
      if (dragHandle.hasPointerCapture(event.pointerId))
        dragHandle.releasePointerCapture(event.pointerId);
      event.preventDefault();
      event.stopPropagation();
    };
    dragHandle.addEventListener("pointerdown", onEditorPointerDown);
    dragHandle.addEventListener("pointermove", onEditorPointerMove);
    dragHandle.addEventListener("pointerup", onEditorPointerUp);
    dragHandle.addEventListener("pointercancel", onEditorPointerUp);

    const repaint = () => {
      for (const target of selected.values()) updateSelectedVisual(target);
      queueEditorLayout();
    };
    // A scrollbar appearing narrows the viewport without a window resize.
    const rootResizeObserver = new ResizeObserver(repaint);

    const contains = (rect, x, y) =>
      x >= rect.x && x <= rect.x + rect.width && y >= rect.y && y <= rect.y + rect.height;
    const removeTargetAtPoint = (x, y) => {
      for (const target of Array.from(selected.values()).reverse()) {
        if (contains(rectFromDomRect(target.element.getBoundingClientRect()), x, y)) {
          removeSelected(target);
          updateStatus();
          return;
        }
      }
      const regionIndex = regions.findIndex((region) => contains(region.rect, x, y));
      if (regionIndex >= 0) {
        const [removed] = regions.splice(regionIndex, 1);
        root.querySelector(`[data-region-id="${removed.id}"]`)?.remove();
        updateStatus();
        return;
      }
      const strokeIndex = strokes.findIndex((stroke) => contains(stroke.bounds, x, y));
      if (strokeIndex >= 0) {
        const [removed] = strokes.splice(strokeIndex, 1);
        svg.querySelector(`[data-stroke-id="${removed.id}"]`)?.remove();
        updateStatus();
      }
    };
    const selectElementsInRect = (rect) => {
      const candidates = Array.from(document.querySelectorAll("body *"))
        .filter((element) => !isAnnotationNode(element))
        .map((element) => ({ element, rect: element.getBoundingClientRect() }))
        .filter(({ rect: candidate }) => {
          if (candidate.width < 2 || candidate.height < 2) return false;
          return !(
            candidate.right < rect.x ||
            candidate.left > rect.x + rect.width ||
            candidate.bottom < rect.y ||
            candidate.top > rect.y + rect.height
          );
        })
        .filter(({ element, rect: candidate }) => {
          const centerX = candidate.left + candidate.width / 2;
          const centerY = candidate.top + candidate.height / 2;
          return (
            contains(rect, centerX, centerY) &&
            (element.children.length === 0 ||
              element instanceof HTMLButtonElement ||
              element instanceof HTMLAnchorElement ||
              element.getAttribute("role") === "button")
          );
        })
        .sort(
          (left, right) =>
            left.rect.width * left.rect.height - right.rect.width * right.rect.height,
        )
        .slice(0, MAX_MARQUEE_ELEMENTS);
      for (const candidate of candidates) addSelected(candidate.element);
      return candidates.length;
    };

    const clearHoverOutline = () => {
      hoverOutline.style.display = "none";
    };
    const onPointerMove = (event) => {
      if (isAnnotationNode(event.target)) {
        clearHoverOutline();
        return;
      }
      if (tool === "select" && dragStart === null) {
        const target = pickFromPoint(event.clientX, event.clientY);
        if (target) positionBox(hoverOutline, visibleElementRect(target));
        else clearHoverOutline();
        return;
      }
      clearHoverOutline();
      if (tool === "marquee" && dragStart) {
        positionBox(
          marqueeBox,
          normalizeRect(dragStart.x, dragStart.y, event.clientX, event.clientY),
        );
        return;
      }
      if (tool === "draw" && activeStroke) {
        activeStroke.target.points = [
          ...activeStroke.target.points,
          { x: event.clientX, y: event.clientY },
        ];
        activeStroke.target.bounds = strokeBounds(
          activeStroke.target.points,
          activeStroke.target.width,
        );
        activeStroke.path.setAttribute("d", pathFromPoints(activeStroke.target.points));
      }
    };
    const onPointerDown = (event) => {
      if (event.button !== 0 || isAnnotationNode(event.target)) return;
      event.preventDefault();
      event.stopPropagation();
      if (tool === "select") {
        const target = pickFromPoint(event.clientX, event.clientY);
        if (target) toggleSelected(target, event.shiftKey);
        return;
      }
      if (tool === "erase") {
        removeTargetAtPoint(event.clientX, event.clientY);
        return;
      }
      dragStart = { x: event.clientX, y: event.clientY };
      if (tool === "draw") {
        const stroke = {
          id: nextId("stroke"),
          color: typeof theme?.primary === "string" ? theme.primary : "#2563eb",
          width: 4,
          points: [dragStart],
          bounds: { x: dragStart.x, y: dragStart.y, width: 1, height: 1 },
        };
        const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
        path.setAttribute(OVERLAY_ATTRIBUTE, "");
        path.setAttribute("data-stroke-id", stroke.id);
        path.setAttribute("fill", "none");
        path.setAttribute("stroke", stroke.color);
        path.setAttribute("stroke-width", String(stroke.width));
        path.setAttribute("stroke-linecap", "round");
        path.setAttribute("stroke-linejoin", "round");
        svg.appendChild(path);
        activeStroke = { target: stroke, path };
      }
    };
    const onPointerUp = (event) => {
      if (!dragStart) return;
      event.preventDefault();
      event.stopPropagation();
      if (tool === "marquee") {
        const rect = normalizeRect(dragStart.x, dragStart.y, event.clientX, event.clientY);
        marqueeBox.style.display = "none";
        if (isUsableRect(rect) && selectElementsInRect(rect) === 0) {
          const region = { id: nextId("region"), rect };
          regions.push(region);
          const regionBox = createBox(
            PRIMARY,
            "color-mix(in srgb, var(--t3-primary) 6%, transparent)",
          );
          regionBox.setAttribute("data-region-id", region.id);
          positionBox(regionBox, rect);
          root.appendChild(regionBox);
        }
      } else if (tool === "draw" && activeStroke) {
        if (activeStroke.target.points.length > 1) strokes.push(activeStroke.target);
        else activeStroke.path.remove();
        activeStroke = null;
      }
      dragStart = null;
      updateStatus();
    };
    const onClick = (event) => {
      if (isAnnotationNode(event.target)) return;
      event.preventDefault();
      event.stopPropagation();
    };
    const stopAtOverlay = (event) => event.stopPropagation();
    // Focus moving from the page into the overlay targets the page element, so the host never sees it.
    const onFocusOutToOverlay = (event) => {
      if (event.relatedTarget instanceof Element && isAnnotationNode(event.relatedTarget))
        event.stopPropagation();
    };
    const onPointerOut = (event) => {
      if (event.relatedTarget === null) clearHoverOutline();
    };
    const onKeyDown = (event) => {
      if (isAnnotationNode(event.target) && event.key !== "Escape") return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        teardown(true);
        return;
      }
      if (event.key === "v") tool = "select";
      else if (event.key === "r") tool = "marquee";
      else if (event.key === "d") tool = "draw";
      else if (event.key === "e") tool = "erase";
      else return;
      refreshToolButtons();
    };

    const teardown = (notifyApp) => {
      if (finished) return;
      finished = true;
      rootResizeObserver.disconnect();
      for (const target of selected.values()) restoreTargetStyles(target);
      window.removeEventListener("pointermove", onPointerMove, true);
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("pointerup", onPointerUp, true);
      window.removeEventListener("pointerout", onPointerOut, true);
      window.removeEventListener("click", onClick, true);
      window.removeEventListener("blur", clearHoverOutline);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("scroll", repaint, true);
      window.removeEventListener("resize", repaint);
      for (const type of PAGE_ISOLATED_EVENTS) host.removeEventListener(type, stopAtOverlay);
      document.documentElement.removeEventListener("focusout", onFocusOutToOverlay);
      if (editorLayoutFrame !== null) window.cancelAnimationFrame(editorLayoutFrame);
      document.documentElement.removeAttribute("data-t3code-annotation-tool");
      cursorStyle.remove();
      inputLayer?.remove();
      baselineFrame?.remove();
      baselineFrame = null;
      baselines.clear();
      host.remove();
      if (active?.session === session) active = null;
      if (notifyApp) post({ type: "cancel" });
    };

    const submitAnnotation = (submission) => {
      if (finished || (submission === "send" && !sendEnabled)) return;
      if (pendingCapture || !hasTargets()) return;
      pendingCapture = true;
      submit.disabled = true;
      submit.textContent = "Capturing…";
      const styleChangesSnapshot = Array.from(styleChanges.values(), (change) => ({ ...change }));
      const elements = Array.from(selected.values(), (target) => {
        const element = captureElement(target.element);
        for (const change of styleChangesSnapshot) {
          if (change.targetId === target.id && element.selector !== null)
            change.selector = element.selector;
        }
        return {
          id: target.id,
          element,
          rect: rectFromDomRect(target.element.getBoundingClientRect()),
        };
      });
      const annotation = {
        id: nextId("annotation"),
        pageUrl: location.href,
        pageTitle: document.title?.trim() || null,
        comment: comment.value.trim(),
        elements,
        regions: [...regions],
        strokes: [...strokes],
        styleChanges: styleChangesSnapshot,
        screenshot: null,
        createdAt: new Date().toISOString(),
      };
      editor.style.display = "none";
      toolbar.style.display = "none";
      hoverOutline.style.display = "none";
      const cropRect = unionRects([
        ...elements.map((target) => target.rect),
        ...annotation.regions.map((region) => region.rect),
        ...annotation.strokes.map((stroke) => stroke.bounds),
      ]);
      // The app reads the screen, so the hidden chrome must be painted away first.
      window.requestAnimationFrame(() =>
        window.requestAnimationFrame(() => {
          if (finished) return;
          post({
            type: "submit",
            annotation,
            cropRect,
            submission,
            viewport: { width: window.innerWidth, height: window.innerHeight },
          });
        }),
      );
    };
    submit.addEventListener("click", (event) => {
      if (event.isTrusted) submitAnnotation("attach");
    });
    root.addEventListener("keydown", (event) => {
      const submission =
        event.target === comment && event.isTrusted ? resolveSubmission(event, sendEnabled) : null;
      // Bubble phase lets editor inputs handle events before hiding them from the page.
      event.stopImmediatePropagation();
      if (!submission) return;
      event.preventDefault();
      submitAnnotation(submission);
    });

    window.addEventListener("pointermove", onPointerMove, { capture: true, passive: false });
    window.addEventListener("pointerdown", onPointerDown, { capture: true, passive: false });
    window.addEventListener("pointerup", onPointerUp, { capture: true, passive: false });
    window.addEventListener("pointerout", onPointerOut, { capture: true, passive: true });
    window.addEventListener("click", onClick, { capture: true, passive: false });
    window.addEventListener("blur", clearHoverOutline);
    window.addEventListener("keydown", onKeyDown, { capture: true });
    window.addEventListener("scroll", repaint, { capture: true, passive: true });
    window.addEventListener("resize", repaint, { passive: true });
    // Bubble phase, so the overlay's own handlers still see these events.
    for (const type of PAGE_ISOLATED_EVENTS)
      host.addEventListener(type, stopAtOverlay, { passive: true });
    document.documentElement.addEventListener("focusout", onFocusOutToOverlay);
    if (inputLayer) {
      document.documentElement.appendChild(inputLayer);
      inputLayer.focus({ preventScroll: true });
    }
    document.documentElement.appendChild(host);
    rootResizeObserver.observe(document.documentElement);
    refreshToolButtons();
    updateStatus();
    active = {
      session,
      teardown,
      setSendEnabled: (enabled) => {
        sendEnabled = enabled;
        updateSendHint();
      },
    };
  };

  Object.defineProperty(window, "__t3Annotation", {
    value: Object.freeze({
      start: (options) => {
        if (options && typeof options.session === "string") startAnnotation(options);
      },
      cancel: () => active?.teardown(false),
      captured: (session) => {
        if (active?.session === session) active.teardown(false);
      },
      setSendEnabled: (enabled) => active?.setSendEnabled(enabled === true),
    }),
  });
})();
