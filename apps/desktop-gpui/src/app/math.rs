//! TeX formulas as images (`MarkdownMath` and `MathTypeset`). The diagram renderer's hidden page
//! also runs a bundled `MathJax`, whose SVG output draws every glyph as a path in the TeX fonts
//! KaTeX's own are cut from, so GPUI's SVG renderer shows it with no math fonts installed.
//! Formulas are cached per expression; one that does not parse keeps its TeX source.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::sync::Arc;

use gpui_kit::component::text::{InlineElement, InlineRenderContext};
use gpui_kit::{
    AnyElement, App, Context, ElementId, Hsla, Image, ImageFormat, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Size, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div, img,
    px, relative, size,
};

use super::Workspace;

/// `MathJax` 3.2.2's `tex-svg-full` build (Apache-2.0, `assets/vendor/mathjax-LICENSE`).
const MATHJAX_GZ: &[u8] = include_bytes!("../../assets/vendor/mathjax-3.2.2.tex-svg-full.js.gz");

/// `window.t3Math(id, tex, display)`, served as `/math.js`: loads `MathJax` on the first
/// formula, typesets with KaTeX's limits (no links, nothing loaded), and reports the SVG with
/// its width, height and ascent in em. A formula that does not parse reports `untypeset`.
pub(super) const MATH_SCRIPT: &str = r#""use strict";
(() => {
  const post = (message) => window.ipc.postMessage(JSON.stringify(message));
  // KaTeX's commands. `newcommand` stays out: MathJax keeps `\def` and `\newcommand` definitions
  // from one formula to the next, where KaTeX scopes them to one, so those keep their source.
  const PACKAGES = ["base", "ams", "amscd", "boldsymbol", "braket", "cancel", "color", "configmacros",
    "gensymb", "textmacros", "verb"];
  const FONT = '"Segoe UI", "Helvetica Neue", "Noto Sans", "DejaVu Sans", "Liberation Sans", Arial, sans-serif';
  let loading = null;
  const load = () => {
    loading = loading || new Promise((resolve, reject) => {
      window.MathJax = {
        tex: {
          packages: PACKAGES,
          macros: { bm: ["\\boldsymbol{#1}", 1] },
          formatError: (_, error) => { throw error; },
        },
        svg: { fontCache: "none", mtextFont: FONT, merrorFont: FONT },
        options: { enableMenu: false },
        startup: { typeset: false },
      };
      const script = document.createElement("script");
      script.src = "/mathjax.js";
      script.onload = () => {
        const startup = window.MathJax.startup;
        if (startup && startup.promise) startup.promise.then(resolve, reject);
        else reject(new Error("MathJax did not start."));
      };
      script.onerror = reject;
      document.head.append(script);
    });
    return loading;
  };
  const clean = (svg) => {
    for (const link of svg.querySelectorAll("a")) link.replaceWith(...link.childNodes);
    for (const element of svg.querySelectorAll("image, foreignObject, style, script, use")) element.remove();
    for (const element of [svg, ...svg.querySelectorAll("*")]) {
      for (const { name } of [...element.attributes]) {
        const lower = name.toLowerCase();
        if (lower.startsWith("on") || lower.startsWith("data-") || lower.endsWith("href") || lower === "src") {
          element.removeAttribute(name);
        }
      }
    }
  };
  window.t3Math = (id, tex, display) => {
    load().then(() => {
      if (typeof MathJax.texReset === "function") MathJax.texReset();
      const svg = MathJax.tex2svg(tex, { display, em: 16, ex: 8 }).querySelector("svg");
      if (!svg) throw new Error("Nothing was typeset.");
      const params = MathJax.startup.output && MathJax.startup.output.font && MathJax.startup.output.font.params;
      const xHeight = (params && params.x_height) || 0.442;
      // MathJax writes lengths in ex.
      const em = (value) => (parseFloat(value) || 0) * xHeight;
      let width, height, ascent;
      const box = (svg.getAttribute("viewBox") || "").trim().split(/\s+/).map(Number);
      if (box.length === 4 && box.every(Number.isFinite)) {
        [width, height, ascent] = [box[2] / 1000, box[3] / 1000, -box[1] / 1000];
      } else {
        // A tag makes the formula 100% wide, at least its min-width, drawn in px at 1ex = 8px.
        const unit = 8 / xHeight;
        width = em(svg.style.minWidth);
        height = em(svg.getAttribute("height"));
        ascent = height + em(svg.style.verticalAlign);
        svg.setAttribute("viewBox", `0 0 ${width * unit} ${height * unit}`);
      }
      clean(svg);
      for (const name of ["width", "height", "style", "role", "focusable"]) svg.removeAttribute(name);
      post({ type: "typeset", id, svg: new XMLSerializer().serializeToString(svg), width, height, ascent });
    }).catch(() => post({ type: "untypeset", id }));
  };
})();
"#;

/// Formulas kept, as the web's typeset cache.
const CACHE_LIMIT: usize = 1000;
/// `.katex { font-size: 1.21em }`.
const MATH_SCALE: f32 = 1.21;
/// `.chat-markdown`'s `text-sm`, which display math sizes from.
const DISPLAY_TEXT_REM: f32 = 0.875;
/// `.katex { line-height: 1.2 }`: a display formula's shortest line.
const DISPLAY_LINE: f32 = 1.2;
/// `MarkdownMath`'s `px-0.5 py-0.75`.
const PAD_X: f32 = 2.;
const PAD_Y: f32 = 3.;

pub(super) fn mathjax_script() -> &'static [u8] {
    static SCRIPT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    SCRIPT.get_or_init(|| {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(MATHJAX_GZ), &mut out);
        out
    })
}

/// A typeset formula's extent, in em of the math font.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Metrics {
    pub width: f32,
    pub height: f32,
    /// From the top to the baseline.
    pub ascent: f32,
}

#[derive(Clone)]
pub(super) struct Typeset {
    /// Drawn in `currentColor`, at no fixed size.
    svg: Arc<str>,
    metrics: Metrics,
}

impl Typeset {
    pub(super) fn new(svg: String, metrics: Metrics) -> Self {
        Self { svg: svg.into(), metrics }
    }
}

enum Formula {
    Pending,
    Ready(Typeset),
    /// The TeX does not parse: it shows as source.
    Failed,
}

/// Typeset formulas by [`formula_key`], and the markdown views waiting on them.
#[derive(Default)]
pub(super) struct Formulas {
    cache: HashMap<u64, Formula>,
    waiting: HashMap<u64, Vec<SharedString>>,
    /// Each drawn formula's image, by formula, size and colour.
    images: HashMap<(u64, [u32; 6]), Arc<Image>>,
}

impl Formulas {
    /// Records what became of `id`; `None` is a formula that keeps its source.
    pub(super) fn settle(&mut self, id: u64, typeset: Option<Typeset>) -> Vec<SharedString> {
        if let Some(entry) = self.cache.get_mut(&id).filter(|entry| matches!(entry, Formula::Pending)) {
            *entry = typeset.map_or(Formula::Failed, Formula::Ready);
        }
        self.waiting.remove(&id).unwrap_or_default()
    }

    /// Forgets formulas still on their way, so the next draw asks again.
    pub(super) fn abandon(&mut self) {
        self.cache.retain(|_, formula| !matches!(formula, Formula::Pending));
        self.waiting.clear();
    }

    /// The formula's image at `size`, in `color`.
    fn image(&mut self, id: u64, typeset: &Typeset, size: Size<f32>, color: Hsla) -> Arc<Image> {
        let key = (id, [size.width, size.height, color.h, color.s, color.l, color.a].map(f32::to_bits));
        if self.images.len() >= CACHE_LIMIT && !self.images.contains_key(&key) {
            self.images.clear();
        }
        self.images
            .entry(key)
            .or_insert_with(|| {
                let svg = sized_svg(&typeset.svg, size, &super::mermaid::css_color(color));
                Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))
            })
            .clone()
    }
}

/// The cache key of a formula: its TeX, inline or displayed.
fn formula_key(tex: &str, display: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    ("math", tex, display).hash(&mut hasher);
    hasher.finish()
}

fn typeset_call(id: u64, tex: &str, display: bool) -> String {
    let tex = serde_json::to_string(tex).unwrap_or_default();
    format!("window.t3Math(\"{id:x}\", {tex}, {display})")
}

/// The SVG at `size` px, its `currentColor` as `color`, since GPUI draws it outside any text.
fn sized_svg(svg: &str, size: Size<f32>, color: &str) -> String {
    let sized = format!("<svg width=\"{}\" height=\"{}\"", size.width, size.height);
    svg.replacen("<svg", &sized, 1).replace("currentColor", color)
}

/// The surrounding text's line, which an inline formula's box is measured against.
#[derive(Clone, Copy)]
struct Strut {
    line_height: f32,
    ascent: f32,
    descent: f32,
    x_height: f32,
}

#[derive(Debug, PartialEq)]
struct InlineBox {
    width: f32,
    height: f32,
    /// Where the formula's top sits inside the box.
    top: f32,
    baseline: f32,
    formula: Size<f32>,
}

/// `MarkdownMath`'s inline box: the formula on a line of the surrounding text, padded, and
/// `align-middle`: its middle half an x-height above the text's baseline.
fn inline_box(metrics: Metrics, font_size: f32, strut: Strut) -> InlineBox {
    let em = font_size * MATH_SCALE;
    let formula = size(metrics.width * em, metrics.height * em);
    let ascent = metrics.ascent * em;
    let above = (strut.line_height - strut.ascent - strut.descent) / 2. + strut.ascent;
    let top = ascent.max(above);
    let bottom = (formula.height - ascent).max(strut.line_height - above);
    let height = top + bottom + 2. * PAD_Y;
    InlineBox {
        width: formula.width + 2. * PAD_X,
        height,
        top: PAD_Y + top - ascent,
        baseline: height / 2. + strut.x_height / 2.,
        formula,
    }
}

/// A display formula's size and line height (`.katex-display`), at `rem` px to the rem.
fn display_box(metrics: Metrics, rem: f32) -> (Size<f32>, f32) {
    let em = rem * DISPLAY_TEXT_REM * MATH_SCALE;
    let formula = size(metrics.width * em, metrics.height * em);
    (formula, formula.height.max(DISPLAY_LINE * em))
}

impl Workspace {
    /// `tex` typeset, asking for it the first time; `None` while it is on its way, when the
    /// views drawing it for `key` re-lay out once it lands, and when it does not parse.
    fn formula(
        &mut self,
        tex: &str,
        display: bool,
        key: &SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(u64, Typeset)> {
        let id = formula_key(tex, display);
        let formulas = &mut self.formulas;
        if !formulas.cache.contains_key(&id) {
            if formulas.cache.len() >= CACHE_LIMIT {
                formulas.cache.retain(|_, formula| matches!(formula, Formula::Pending));
                formulas.images.clear();
            }
            formulas.cache.insert(id, Formula::Pending);
            self.render_in_page(id, typeset_call(id, tex, display), window, cx);
        }
        let formulas = &mut self.formulas;
        match formulas.cache.get(&id) {
            Some(Formula::Ready(typeset)) => Some((id, typeset.clone())),
            Some(Formula::Pending) => {
                let waiting = formulas.waiting.entry(id).or_default();
                if !waiting.contains(key) {
                    waiting.push(key.clone());
                }
                None
            }
            Some(Formula::Failed) | None => None,
        }
    }
}

/// `$…$` typeset in the surrounding text's size and colour; `None` draws its source.
pub(super) fn inline_formula(
    this: &WeakEntity<Workspace>,
    key: &SharedString,
    tex: &str,
    context: &InlineRenderContext,
    window: &mut Window,
    cx: &mut App,
) -> Option<InlineElement> {
    let style = context.text_style();
    let (font_size, color) = (context.font_size(), style.color);
    let text = window.text_system();
    let font = text.resolve_font(&style.font());
    let x_height = f32::from(text.x_height(font, font_size));
    let strut = Strut {
        line_height: f32::from(context.line_height()),
        ascent: f32::from(text.ascent(font, font_size)).abs(),
        descent: f32::from(text.descent(font, font_size)).abs(),
        x_height: if x_height > 0. { x_height } else { f32::from(font_size) / 2. },
    };
    let (laid, image) = this
        .update(cx, |workspace, cx| {
            let (id, typeset) = workspace.formula(tex, false, key, window, cx)?;
            let laid = inline_box(typeset.metrics, f32::from(font_size), strut);
            let image = workspace.formulas.image(id, &typeset, laid.formula, color);
            Some((laid, image))
        })
        .ok()
        .flatten()?;
    let element = div()
        .flex_none()
        .w(px(laid.width))
        .h(px(laid.height))
        .pl(px(PAD_X))
        .pt(px(laid.top))
        .child(img(image).flex_none().w(px(laid.formula.width)).h(px(laid.formula.height)));
    Some(InlineElement::new(element).with_baseline(px(laid.baseline)))
}

/// A display formula (`\[…\]`, `$$…$$` or a closed `math` fence), centered and scrolling
/// sideways when wider than the column; its source until typeset, and when it does not parse.
/// `at` tells apart the formulas of one message.
pub(super) fn display_formula(
    this: &WeakEntity<Workspace>,
    key: &SharedString,
    tex: &str,
    at: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let tex = tex.trim();
    let (rem, color) = (f32::from(window.rem_size()), window.text_style().color);
    let typeset = this
        .update(cx, |workspace, cx| {
            let (id, typeset) = workspace.formula(tex, true, key, window, cx)?;
            let (formula, line) = display_box(typeset.metrics, rem);
            Some((workspace.formulas.image(id, &typeset, formula, color), formula, line))
        })
        .ok()
        .flatten();
    let body = div().id(ElementId::Name(format!("math:{key}:{at:x}").into())).w_full().px(px(PAD_X)).py(px(PAD_Y));
    match typeset {
        Some((image, formula, line)) => body
            .flex()
            .overflow_x_scroll()
            .child(
                div().flex_none().min_w(relative(1.)).flex().justify_center().child(
                    div()
                        .flex_none()
                        .h(px(line))
                        .pt(px((line - formula.height) / 2.))
                        .child(img(image).flex_none().w(px(formula.width)).h(px(formula.height))),
                ),
            )
            .into_any_element(),
        None => body.child(format!("\\[\n{tex}\n\\]")).into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_are_keyed_by_tex_and_mode() {
        assert_eq!(formula_key("x^2", false), formula_key("x^2", false));
        assert_ne!(formula_key("x^2", false), formula_key("x^2", true), "display math typesets apart");
        assert_ne!(
            formula_key("graph TD", false),
            crate::app::mermaid::diagram_key("graph TD", "", false),
            "formulas and diagrams share the page's ids"
        );
        assert_eq!(
            typeset_call(0x1f, "\\frac{a}{b}\"</script>", true),
            r#"window.t3Math("1f", "\\frac{a}{b}\"</script>", true)"#
        );
    }

    #[test]
    fn formulas_settle_once_and_wake_their_views() {
        let mut formulas = Formulas::default();
        formulas.cache.insert(1, Formula::Pending);
        formulas.waiting.insert(1, vec!["message".into()]);
        let metrics = Metrics { width: 1., height: 1., ascent: 0.75 };
        assert_eq!(
            formulas.settle(1, Some(Typeset::new("<svg/>".into(), metrics))),
            vec![SharedString::from("message")]
        );
        assert!(formulas.settle(1, None).is_empty());
        assert!(matches!(formulas.cache.get(&1), Some(Formula::Ready(_))), "a late failure keeps the formula");
        formulas.cache.insert(2, Formula::Pending);
        formulas.waiting.insert(2, vec!["other".into()]);
        formulas.abandon();
        assert!(!formulas.cache.contains_key(&2), "a pending formula is asked for again");
        assert!(formulas.waiting.is_empty());
        assert!(formulas.cache.contains_key(&1));
    }

    #[test]
    fn the_svg_takes_its_size_and_colour() {
        assert_eq!(
            sized_svg(
                r#"<svg viewBox="0 0 10 10"><g fill="currentColor" stroke="currentColor"/></svg>"#,
                size(12.5, 8.),
                "#abc"
            ),
            r##"<svg width="12.5" height="8" viewBox="0 0 10 10"><g fill="#abc" stroke="#abc"/></svg>"##
        );
        let mut formulas = Formulas::default();
        let typeset = Typeset::new("<svg/>".into(), Metrics { width: 1., height: 1., ascent: 1. });
        let red = gpui_kit::red();
        let first = formulas.image(1, &typeset, size(4., 4.), red);
        assert!(Arc::ptr_eq(&first, &formulas.image(1, &typeset, size(4., 4.), red)), "drawn again, it is reused");
        assert!(!Arc::ptr_eq(&first, &formulas.image(1, &typeset, size(4., 4.), gpui_kit::blue())));
    }

    #[test]
    fn inline_formulas_center_on_the_line() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        let strut = Strut { line_height: 20., ascent: 13., descent: 3., x_height: 7. };
        // A small formula fits the line: the box is the line, padded. 1em of math is 12.1px.
        let small = inline_box(Metrics { width: 1., height: 0.5, ascent: 0.4 }, 10., strut);
        assert!(close(small.formula.width, 12.1) && close(small.formula.height, 6.05));
        assert!(close(small.width, 16.1), "padded 2px a side");
        assert!(close(small.height, 26.), "the 20px line, padded 3px a side");
        assert!(close(small.baseline, 16.5), "its middle half an x-height above the baseline");
        assert!(close(small.top, PAD_Y + 15. - 4.84), "the formula's baseline on the line's");
        // A tall one grows the box both ways and stays centered.
        let tall = inline_box(Metrics { width: 1., height: 3., ascent: 2. }, 10., strut);
        assert!(close(tall.height, 24.2 + 12.1 + 6.));
        assert!(close(tall.top, PAD_Y));
        assert!(close(tall.baseline, tall.height / 2. + 3.5));
    }

    #[test]
    fn display_formulas_keep_katex_line() {
        let (formula, line) = display_box(Metrics { width: 2., height: 0.5, ascent: 0.5 }, 16.);
        assert!((formula.width - 2. * 16.94).abs() < 1e-3);
        assert!((line - 1.2 * 16.94).abs() < 1e-3, "a short formula keeps the line's height");
        let (formula, line) = display_box(Metrics { width: 1., height: 3., ascent: 2. }, 16.);
        assert!((line - formula.height).abs() < 1e-6);
    }

    #[test]
    fn the_vendored_typesetter_decompresses() {
        assert!(mathjax_script().starts_with(b"(function(){\"use strict\";"));
    }
}
