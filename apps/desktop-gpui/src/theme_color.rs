//! The color math of `themePalette.ts`: CSS colors to canonical `oklch(...)` strings, WCAG
//! contrast solves, the guided generator (`createVividThemeColors`) and the advanced editor's
//! derived roles (`updateThemeColorFamily`). Parses hex, `rgb()`, `hsl()`, `oklch()`,
//! `oklab()` and a few keywords: the forms theme files and VS Code themes use (the web
//! accepts any CSS color through culori).

use crate::themes::Roles;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// sRGB channels in 0..=255.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Parsed {
    pub color: Oklch,
    pub alpha: f64,
}

const LIGHT_FOREGROUND: Rgb = Rgb { r: 255., g: 250., b: 255. };
const DARK_FOREGROUND: Rgb = Rgb { r: 36., g: 21., b: 35. };
const WHITE: Rgb = Rgb { r: 255., g: 255., b: 255. };
const BLACK: Rgb = Rgb { r: 0., g: 0., b: 0. };

fn number(part: &str) -> Option<f64> {
    part.parse::<f64>().ok().filter(|value| value.is_finite())
}

/// A component that may be a percentage of `full`.
fn component(part: &str, full: f64) -> Option<f64> {
    match part.strip_suffix('%') {
        Some(percent) => Some(number(percent)? / 100. * full),
        None if part == "none" => Some(0.),
        None => number(part),
    }
}

fn hue(part: &str) -> Option<f64> {
    let part = part.trim_end_matches("deg");
    if part == "none" { Some(0.) } else { number(part) }
}

/// The channels and optional alpha of `name(a b c / d)` or `name(a, b, c, d)`.
fn function_args<'a>(value: &'a str, names: &[&str]) -> Option<(Vec<&'a str>, Option<&'a str>)> {
    let open = value.find('(')?;
    if !names.contains(&value[..open].trim()) {
        return None;
    }
    let body = value[open + 1..].strip_suffix(')')?;
    let (channels, alpha) = match body.split_once('/') {
        Some((channels, alpha)) => (channels, Some(alpha.trim())),
        None => (body, None),
    };
    let mut parts: Vec<&str> = channels.split([',', ' ']).map(str::trim).filter(|part| !part.is_empty()).collect();
    let alpha = match alpha {
        Some(alpha) => Some(alpha),
        None if parts.len() == 4 => parts.pop(),
        None => None,
    };
    (parts.len() == 3).then_some((parts, alpha))
}

fn alpha_of(part: Option<&str>) -> Option<f64> {
    part.map_or(Some(1.), |part| component(part, 1.)).map(|alpha| alpha.clamp(0., 1.))
}

fn parse_hex(hex: &str) -> Option<(Rgb, f64)> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |s: &str| u8::from_str_radix(s, 16).ok().map(f64::from);
    let short = |i: usize| byte(&hex[i..=i].repeat(2));
    match hex.len() {
        3 | 4 => {
            Some((Rgb { r: short(0)?, g: short(1)?, b: short(2)? }, if hex.len() == 4 { short(3)? / 255. } else { 1. }))
        }
        6 | 8 => Some((
            Rgb { r: byte(&hex[0..2])?, g: byte(&hex[2..4])?, b: byte(&hex[4..6])? },
            if hex.len() == 8 { byte(&hex[6..8])? / 255. } else { 1. },
        )),
        _ => None,
    }
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> Rgb {
    let h = h.rem_euclid(360.);
    let k = |n: f64| (n + h / 30.) % 12.;
    let a = s * l.min(1. - l);
    let f = |n: f64| (l - a * (k(n) - 3.).min(9. - k(n)).clamp(-1., 1.)) * 255.;
    Rgb { r: f(0.), g: f(8.), b: f(4.) }
}

fn oklab_to_oklch(l: f64, a: f64, b: f64) -> Oklch {
    Oklch { l, c: a.hypot(b), h: b.atan2(a).to_degrees() }
}

/// `parseThemeColor`: the color in OKLCH, lightness clamped to 0..1, and its alpha.
pub fn parse(value: &str) -> Option<Parsed> {
    let input = value.trim();
    let lower = input.to_ascii_lowercase();
    let (color, alpha) = if let Some(hex) = lower.strip_prefix('#') {
        let (rgb, alpha) = parse_hex(hex)?;
        (rgb_to_oklch(rgb), alpha)
    } else if let Some(keyword) = match lower.as_str() {
        "white" => Some((WHITE, 1.)),
        "black" => Some((BLACK, 1.)),
        "transparent" => Some((BLACK, 0.)),
        _ => None,
    } {
        (rgb_to_oklch(keyword.0), keyword.1)
    } else if let Some((parts, alpha)) = function_args(&lower, &["rgb", "rgba"]) {
        let rgb = Rgb { r: component(parts[0], 255.)?, g: component(parts[1], 255.)?, b: component(parts[2], 255.)? };
        (rgb_to_oklch(rgb), alpha_of(alpha)?)
    } else if let Some((parts, alpha)) = function_args(&lower, &["hsl", "hsla"]) {
        let rgb = hsl_to_rgb(hue(parts[0])?, component(parts[1], 1.)?, component(parts[2], 1.)?);
        (rgb_to_oklch(rgb), alpha_of(alpha)?)
    } else if let Some((parts, alpha)) = function_args(&lower, &["oklch"]) {
        let color = Oklch { l: component(parts[0], 1.)?, c: component(parts[1], 0.4)?, h: hue(parts[2])? };
        (color, alpha_of(alpha)?)
    } else if let Some((parts, alpha)) = function_args(&lower, &["oklab"]) {
        let color = oklab_to_oklch(component(parts[0], 1.)?, component(parts[1], 0.4)?, component(parts[2], 0.4)?);
        (color, alpha_of(alpha)?)
    } else {
        return None;
    };
    [color.l, color.c, color.h, alpha].iter().all(|v| v.is_finite()).then_some(Parsed {
        color: Oklch { l: color.l.clamp(0., 1.), c: color.c.max(0.), h: color.h },
        alpha: alpha.clamp(0., 1.),
    })
}

pub fn is_color(value: &str) -> bool {
    parse(value).is_some()
}

/// `formatThemeColorNumber`: fixed precision without trailing zeros, and never `-0`.
fn format_number(value: f64, precision: i32) -> String {
    let value = if value.abs() < 10f64.powi(-precision) / 2. { 0. } else { value };
    let text = format!("{value:.*}", usize::try_from(precision).unwrap_or(0));
    if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_owned() } else { text }
}

fn format_oklch(color: Oklch, alpha: f64) -> String {
    let hue = if color.c < 0.0000005 { 0. } else { color.h.rem_euclid(360.) };
    let body = format!("{} {} {}", format_number(color.l, 6), format_number(color.c, 6), format_number(hue, 3));
    if alpha < 1. { format!("oklch({body} / {})", format_number(alpha, 4)) } else { format!("oklch({body})") }
}

/// `toCanonicalThemeColor`.
pub fn canonical(value: &str) -> Option<String> {
    parse(value).map(|parsed| format_oklch(parsed.color, parsed.alpha))
}

/// `themeColorToHex`: `#rrggbb`, or `#rrggbbaa` when translucent.
#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "alpha is below 1, so the byte fits")]
pub fn to_hex(value: &str) -> Option<String> {
    let parsed = parse(value)?;
    let opaque = rgb_hex(oklch_to_rgb(parsed.color, true));
    Some(if parsed.alpha >= 1. { opaque } else { format!("{opaque}{:02x}", (parsed.alpha * 255.).round() as u8) })
}

/// The color in sRGB for painting: unrounded channels in 0..=1 and the alpha.
#[expect(clippy::cast_possible_truncation, reason = "colour channels; f32 is plenty")]
pub fn to_srgba(value: &str) -> Option<[f32; 4]> {
    let parsed = parse(value)?;
    let rgb = oklch_to_rgb(parsed.color, false);
    Some([(rgb.r / 255.) as f32, (rgb.g / 255.) as f32, (rgb.b / 255.) as f32, parsed.alpha as f32])
}

fn rgb_of(value: &str, fallback: Rgb) -> Rgb {
    parse(value).map_or(fallback, |parsed| oklch_to_rgb(parsed.color, true))
}

pub fn rgb_hex(color: Rgb) -> String {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255 before the cast")]
    let channel = |value: f64| value.clamp(0., 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", channel(color.r), channel(color.g), channel(color.b))
}

fn rgb_color(color: Rgb) -> String {
    format_oklch(rgb_to_oklch(color), 1.)
}

fn oklch_color(color: Oklch) -> String {
    format_oklch(gamut_map(color), 1.)
}

pub fn mix(base: Rgb, overlay: Rgb, amount: f64) -> Rgb {
    Rgb {
        r: base.r + (overlay.r - base.r) * amount,
        g: base.g + (overlay.g - base.g) * amount,
        b: base.b + (overlay.b - base.b) * amount,
    }
}

/// WCAG relative luminance (the 0.03928 threshold `themePalette.ts` uses).
pub fn luminance(color: Rgb) -> f64 {
    let linear = |channel: f64| {
        let c = channel / 255.;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

pub fn contrast(first: Rgb, second: Rgb) -> f64 {
    let (a, b) = (luminance(first), luminance(second));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// At luminance 0.179 black and white text have equal contrast.
pub fn is_dark(color: Rgb) -> bool {
    luminance(color) < 0.179
}

fn to_linear(channel: f64) -> f64 {
    let c = channel / 255.;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(channel: f64, round: bool) -> f64 {
    let c = if channel <= 0.0031308 { channel * 12.92 } else { 1.055 * channel.powf(1. / 2.4) - 0.055 };
    let srgb = c.clamp(0., 1.) * 255.;
    if round { srgb.round() } else { srgb }
}

pub fn rgb_to_oklch(color: Rgb) -> Oklch {
    let (r, g, b) = (to_linear(color.r), to_linear(color.g), to_linear(color.b));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    oklab_to_oklch(
        0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
    )
}

fn oklch_to_linear(Oklch { l, c, h }: Oklch) -> [f64; 3] {
    let (sin, cos) = h.to_radians().sin_cos();
    let (a, b) = (c * cos, c * sin);
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.291485548 * b).powi(3);
    [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_,
    ]
}

/// `mapThemeOklchToSrgbGamut`: lowers chroma until the color fits sRGB.
fn gamut_map(color: Oklch) -> Oklch {
    let in_gamut = |c: f64| oklch_to_linear(Oklch { c, ..color }).iter().all(|v| (-0.0001..=1.0001).contains(v));
    if in_gamut(color.c) {
        return color;
    }
    let (mut low, mut high) = (0., color.c);
    let resolution = 0.000001f64;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a small, positive step count")]
    let steps = (color.c.max(resolution).log2() - resolution.log2()).ceil().max(1.) as usize;
    for _ in 0..steps {
        let mid = f64::midpoint(low, high);
        if in_gamut(mid) { low = mid } else { high = mid }
    }
    Oklch { c: low, ..color }
}

pub fn oklch_to_rgb(color: Oklch, round: bool) -> Rgb {
    let [r, g, b] = oklch_to_linear(gamut_map(color));
    Rgb { r: from_linear(r, round), g: from_linear(g, round), b: from_linear(b, round) }
}

/// `solveOklchLightness`: moves only the lightness until `base` reaches `min` contrast on all of `against`.
fn solve_lightness(base: Oklch, against: &[Rgb], min: f64, lighter: bool) -> Oklch {
    let contrast_of = |color: Oklch| {
        let rgb = oklch_to_rgb(color, true);
        against.iter().map(|surface| contrast(rgb, *surface)).fold(f64::INFINITY, f64::min)
    };
    if contrast_of(base) >= min {
        return base;
    }
    let (mut low, mut high) = if lighter { (base.l, 1.) } else { (0., base.l) };
    for _ in 0..18 {
        let mid = f64::midpoint(low, high);
        let passes = contrast_of(Oklch { l: mid, ..base }) >= min;
        if passes == lighter { high = mid } else { low = mid }
    }
    Oklch { l: if lighter { high } else { low }, ..base }
}

/// `readableThemeColorOn`: the color with its lightness moved until it reads on `surface`.
pub fn readable_on(color: &str, surface: &str, min: f64) -> String {
    let (Some(parsed), Some(surface)) = (parse(color), parse(surface)) else { return color.to_owned() };
    let surface = oklch_to_rgb(surface.color, true);
    if contrast(oklch_to_rgb(parsed.color, true), surface) >= min {
        return color.to_owned();
    }
    let solved = solve_lightness(parsed.color, &[surface], min, is_dark(surface));
    rgb_hex(oklch_to_rgb(solved, true))
}

fn readable_foreground(background: Rgb) -> Rgb {
    let (light, dark) = (contrast(background, LIGHT_FOREGROUND), contrast(background, DARK_FOREGROUND));
    if light.max(dark) >= 4.5 {
        return if light >= dark { LIGHT_FOREGROUND } else { DARK_FOREGROUND };
    }
    if contrast(background, WHITE) >= contrast(background, BLACK) { WHITE } else { BLACK }
}

fn readable_text(background: Rgb, foreground: Rgb, amount: f64, min: f64) -> Rgb {
    let softened = mix(foreground, background, amount);
    if contrast(softened, background) >= min {
        return softened;
    }
    let (mut readable, mut lower, mut upper) = (foreground, 0., amount);
    for _ in 0..12 {
        let candidate_amount = f64::midpoint(lower, upper);
        let candidate = mix(foreground, background, candidate_amount);
        if contrast(candidate, background) >= min {
            readable = candidate;
            lower = candidate_amount;
        } else {
            upper = candidate_amount;
        }
    }
    readable
}

fn standard_muted_text(background: Rgb, foreground: Rgb) -> Rgb {
    readable_text(background, foreground, 1., if is_dark(background) { 5.082 } else { 4.705 })
}

/// `standardStatusColors`.
fn standard_status_colors(canvas: Rgb, colors: &mut Roles) {
    let dark = is_dark(canvas);
    let (error, error_fg, warning, warning_fg) =
        if dark { ("#fb414a", "#ff6467", "#fe9a00", "#ffb900") } else { ("#fb2c36", "#c10007", "#fe9a00", "#bb4d00") };
    let surface_of = |value: &str| mix(canvas, rgb_of(value, canvas), if dark { 0.16 } else { 0.08 });
    let readable = |foreground: &str, surface: Rgb| {
        oklch_color(solve_lightness(rgb_to_oklch(rgb_of(foreground, canvas)), &[surface], 4.6, dark))
    };
    let (error_surface, warning_surface) = (surface_of(error), surface_of(warning));
    colors.set("error", canonical(error).unwrap_or_default());
    colors.set("errorForeground", readable(error_fg, error_surface));
    colors.set("errorSurface", rgb_color(error_surface));
    colors.set("warning", canonical(warning).unwrap_or_default());
    colors.set("warningForeground", readable(warning_fg, warning_surface));
    colors.set("warningSurface", rgb_color(warning_surface));
}

/// `createVividThemeColors`: a whole palette from a background and an accent.
pub fn vivid(dark_appearance: bool, background: &str, accent: &str) -> Roles {
    let mut colors = crate::themes::default_roles(dark_appearance).clone();
    let canvas_rgb = rgb_of(
        background,
        if dark_appearance { Rgb { r: 24., g: 15., b: 27. } } else { Rgb { r: 250., g: 245., b: 250. } },
    );
    let accent_rgb = rgb_of(accent, Rgb { r: 168., g: 67., b: 112. });
    let (canvas, accent) = (rgb_to_oklch(canvas_rgb), rgb_to_oklch(accent_rgb));
    let dark = is_dark(canvas_rgb);
    let hue = if accent.c < 0.02 { canvas.h } else { accent.h };
    let tint = (accent.c * 0.22).clamp(0.008, 0.045);
    let step = if dark { 1. } else { -1. };
    let pick = |on_dark: f64, on_light: f64| if dark { on_dark } else { on_light };
    let at = |delta: f64, c: f64| Oklch { l: (canvas.l + step * delta).clamp(0.05, 0.98), c, h: hue };
    let ac = |factor: f64, max: f64| (accent.c * factor).min(max);

    let text_base = Oklch { l: pick(0.95, 0.2), c: (accent.c * 0.25).min(0.035), h: hue };
    let text_rgb = oklch_to_rgb(solve_lightness(text_base, &[canvas_rgb], 7., dark), true);
    let text_muted = standard_muted_text(canvas_rgb, text_rgb);
    let action = Oklch {
        l: (accent.l + pick(0.06, -0.02)).clamp(0.35, 0.85),
        c: (accent.c * 0.9).max(0.06),
        h: (hue + 50.) % 360.,
    };
    let action_rgb = oklch_to_rgb(action, true);
    let sidebar = at(0.045, tint * 1.4);
    let surface_raised = at(0.05, tint);
    let secondary = at(pick(0.1, 0.06), ac(0.5, 0.09));
    let muted = at(pick(0.06, 0.04), ac(0.35, 0.06));
    let accent_surface = at(pick(0.13, 0.08), ac(0.55, 0.11));
    let message_surface = at(pick(0.16, 0.1), ac(0.6, 0.13));
    let update_surface = at(pick(0.14, 0.09), ac(0.55, 0.12));
    let foreground_on =
        |surface: Oklch| oklch_color(solve_lightness(text_base, &[oklch_to_rgb(surface, true)], 4.6, dark));
    let search_match = oklch_to_rgb(at(0.22, accent.c.min(0.12)), true);
    let search_active = oklch_to_rgb(at(0.4, accent.c.min(0.18)), true);
    let (canvas_color, text, muted_text, accent_color) =
        (rgb_color(canvas_rgb), rgb_color(text_rgb), rgb_color(text_muted), rgb_color(accent_rgb));

    standard_status_colors(canvas_rgb, &mut colors);
    let set = |colors: &mut Roles, pairs: &[(&str, String)]| {
        for (role, value) in pairs {
            colors.set(role, value.clone());
        }
    };
    set(
        &mut colors,
        &[
            ("canvas", canvas_color.clone()),
            ("chrome", canvas_color.clone()),
            ("toolbar", canvas_color.clone()),
            ("toolbarForeground", text.clone()),
            ("toolbarBorder", oklch_color(at(pick(0.14, 0.1), ac(0.4, 0.08)))),
            ("toolbarControl", oklch_color(at(pick(0.09, 0.05), tint * 1.3))),
            ("toolbarControlForeground", text.clone()),
            ("toolbarControlHover", oklch_color(at(pick(0.14, 0.09), tint * 1.6))),
            ("surface", oklch_color(at(0.015, tint))),
            ("surfaceRaised", oklch_color(surface_raised)),
            ("surfaceOverlay", oklch_color(at(0.075, tint))),
            ("text", text.clone()),
            ("textMuted", muted_text.clone()),
            ("border", oklch_color(at(pick(0.16, 0.12), ac(0.35, 0.07)))),
            ("input", oklch_color(at(pick(0.21, 0.16), ac(0.4, 0.08)))),
            ("focus", accent_color.clone()),
            ("accent", accent_color.clone()),
            ("accentForeground", rgb_color(readable_foreground(accent_rgb))),
            ("secondary", oklch_color(secondary)),
            ("secondaryForeground", foreground_on(secondary)),
            ("muted", oklch_color(muted)),
            ("mutedForeground", rgb_color(readable_text(oklch_to_rgb(muted, true), text_rgb, 1., 4.6))),
            ("placeholder", rgb_color(readable_text(oklch_to_rgb(surface_raised, true), text_rgb, 1., 4.6))),
            ("secondaryLabel", muted_text.clone()),
            ("iconMuted", muted_text),
            ("update", accent_color.clone()),
            ("updateForeground", foreground_on(update_surface)),
            ("updateSurface", oklch_color(update_surface)),
            ("accentSurface", oklch_color(accent_surface)),
            ("accentSurfaceForeground", foreground_on(accent_surface)),
            ("messageSurface", oklch_color(message_surface)),
            ("messageForeground", foreground_on(message_surface)),
            ("messageAction", rgb_color(action_rgb)),
            ("messageActionForeground", rgb_color(readable_foreground(action_rgb))),
            ("messageActionHover", oklch_color(Oklch { l: action.l + pick(0.06, -0.06), ..action })),
            ("codeBackground", oklch_color(at(0.035, tint * 0.8))),
            ("codeForeground", text.clone()),
            ("searchMatchBackground", rgb_color(search_match)),
            ("searchMatchForeground", rgb_color(readable_foreground(search_match))),
            ("searchMatchActiveBackground", rgb_color(search_active)),
            ("searchMatchActiveForeground", rgb_color(readable_foreground(search_active))),
            ("sidebar", oklch_color(sidebar)),
            ("sidebarForeground", foreground_on(sidebar)),
            ("sidebarMutedForeground", rgb_color(standard_muted_text(oklch_to_rgb(sidebar, true), text_rgb))),
            ("sidebarControlSurface", oklch_color(at(pick(0.1, 0.07), tint * 1.5))),
            ("sidebarRowHover", oklch_color(at(pick(0.08, 0.06), ac(0.45, 0.08)))),
            ("sidebarRowActive", oklch_color(at(pick(0.12, 0.09), ac(0.55, 0.1)))),
            ("sidebarRowSelected", oklch_color(at(pick(0.14, 0.1), ac(0.6, 0.11)))),
            ("sidebarBorder", oklch_color(at(pick(0.17, 0.12), ac(0.4, 0.08)))),
            ("terminalBackground", canvas_color),
            ("terminalForeground", text),
            ("terminalCursor", accent_color),
            ("terminalSelection", oklch_color(at(pick(0.18, 0.12), ac(0.55, 0.12)))),
            ("terminalScrollbar", oklch_color(at(pick(0.22, 0.16), tint))),
            ("terminalScrollbarHover", oklch_color(at(pick(0.3, 0.22), tint))),
        ],
    );
    colors
}

/// `updateThemeColorFamily`: sets `role` and rederives the roles that follow it.
pub fn update_family(dark_appearance: bool, colors: &Roles, role: &str, value: &str) -> Roles {
    let mut next = colors.clone();
    let Some(selected) = parse(value) else {
        next.set(role, value.to_owned());
        return next;
    };
    let normalized = format_oklch(selected.color, selected.alpha);
    let canvas = rgb_of(
        colors.get("canvas"),
        if dark_appearance { Rgb { r: 24., g: 15., b: 27. } } else { Rgb { r: 250., g: 245., b: 250. } },
    );
    let selected_rgb = oklch_to_rgb(selected.color, true);
    let on = |background: Rgb| mix(background, selected_rgb, selected.alpha);
    let on_canvas = on(canvas);
    let accent = rgb_of(colors.get("accent"), Rgb { r: 168., g: 67., b: 112. });
    let canvas_dark = is_dark(canvas);
    let foreground_on = |background: Rgb| rgb_color(readable_foreground(background));
    let tone_on =
        |background: Rgb| oklch_color(solve_lightness(selected.color, &[background], 4.6, is_dark(background)));
    let status = || {
        let surface = mix(canvas, on_canvas, if canvas_dark { 0.16 } else { 0.08 });
        (tone_on(surface), rgb_color(surface))
    };
    let n = || normalized.clone();
    let pairs: Vec<(&str, String)> = match role {
        "canvas" => vec![("canvas", n()), ("chrome", n()), ("toolbar", n())],
        "text" => vec![("text", n()), ("toolbarForeground", n()), ("toolbarControlForeground", n())],
        "mutedForeground" => {
            ["textMuted", "mutedForeground", "placeholder", "secondaryLabel", "iconMuted", "sidebarMutedForeground"]
                .into_iter()
                .map(|role| (role, n()))
                .collect()
        }
        "border" => vec![("border", n()), ("toolbarBorder", n()), ("sidebarBorder", n())],
        "secondary" => vec![
            ("secondary", n()),
            ("secondaryForeground", foreground_on(on_canvas)),
            ("muted", n()),
            ("toolbarControl", n()),
        ],
        "accentSurface" => vec![
            ("accentSurface", n()),
            ("accentSurfaceForeground", foreground_on(on_canvas)),
            ("toolbarControlHover", n()),
        ],
        "accent" => {
            let update_surface = mix(canvas, on_canvas, if canvas_dark { 0.32 } else { 0.16 });
            vec![
                ("accent", n()),
                ("accentForeground", foreground_on(on_canvas)),
                ("focus", n()),
                ("update", n()),
                ("updateForeground", tone_on(update_surface)),
                ("updateSurface", rgb_color(update_surface)),
                ("terminalCursor", n()),
            ]
        }
        "messageAction" => {
            let foreground = readable_foreground(on_canvas);
            let toward = if foreground == LIGHT_FOREGROUND || foreground == WHITE { BLACK } else { WHITE };
            let hover = mix(selected_rgb, toward, 0.12);
            vec![
                ("messageAction", n()),
                ("messageActionForeground", rgb_color(foreground)),
                ("messageActionHover", format_oklch(rgb_to_oklch(hover), selected.alpha)),
            ]
        }
        "messageSurface" => vec![("messageSurface", n()), ("messageForeground", foreground_on(on_canvas))],
        "searchMatchBackground" => {
            vec![("searchMatchBackground", rgb_color(on_canvas)), ("searchMatchForeground", foreground_on(on_canvas))]
        }
        "searchMatchActiveBackground" => vec![
            ("searchMatchActiveBackground", rgb_color(on_canvas)),
            ("searchMatchActiveForeground", foreground_on(on_canvas)),
        ],
        "codeBackground" => vec![("codeBackground", n()), ("codeForeground", foreground_on(on_canvas))],
        "sidebar" => vec![("sidebar", n()), ("sidebarForeground", foreground_on(on_canvas))],
        "sidebarRowSelected" => {
            let sidebar = rgb_of(colors.get("sidebar"), canvas);
            let on_sidebar = on(sidebar);
            vec![
                ("sidebarRowHover", rgb_color(mix(sidebar, on_sidebar, 0.5))),
                ("sidebarRowActive", rgb_color(mix(sidebar, on_sidebar, 0.8))),
                ("sidebarRowSelected", n()),
            ]
        }
        "terminalBackground" => {
            let terminal_dark = is_dark(on_canvas);
            let foreground = readable_foreground(on_canvas);
            let pick = |on_dark: f64, on_light: f64| if terminal_dark { on_dark } else { on_light };
            vec![
                ("terminalBackground", n()),
                ("terminalForeground", rgb_color(foreground)),
                ("terminalSelection", rgb_color(mix(on_canvas, accent, pick(0.35, 0.18)))),
                ("terminalScrollbar", rgb_color(mix(on_canvas, foreground, pick(0.42, 0.22)))),
                ("terminalScrollbarHover", rgb_color(mix(on_canvas, foreground, pick(0.55, 0.32)))),
            ]
        }
        "error" | "warning" => {
            let (foreground, surface) = status();
            vec![
                (role, n()),
                (if role == "error" { "errorForeground" } else { "warningForeground" }, foreground),
                (if role == "error" { "errorSurface" } else { "warningSurface" }, surface),
            ]
        }
        _ => vec![(role, n())],
    };
    for (role, value) in pairs {
        next.set(role, value);
    }
    next
}

/// `hexToHsv` (`lib/color.ts`): hue in degrees, saturation and value in 0..=1.
#[expect(clippy::float_cmp, reason = "`max` is one of r, g and b, so equality is exact")]
pub fn hex_to_hsv(hex: &str) -> [f32; 3] {
    let value = u32::from_str_radix(hex.trim_start_matches('#').get(..6).unwrap_or("000000"), 16).unwrap_or(0);
    let [r, g, b] = [16, 8, 0].map(|shift| ((value >> shift) & 255) as f32 / 255.);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let delta = max - min;
    let mut hue = 0.;
    if delta != 0. {
        hue = if max == r {
            ((g - b) / delta) % 6.
        } else if max == g {
            (b - r) / delta + 2.
        } else {
            (r - g) / delta + 4.
        } * 60.;
        if hue < 0. {
            hue += 360.;
        }
    }
    [hue, if max == 0. { 0. } else { delta / max }, max]
}

/// `hsvToHex`.
pub fn hsv_to_hex([hue, saturation, value]: [f32; 3]) -> String {
    let h = hue.rem_euclid(360.);
    let chroma = value * saturation;
    let x = chroma * (1. - ((h / 60.) % 2. - 1.).abs());
    let m = value - chroma;
    let (r, g, b) = match h {
        h if h < 60. => (chroma, x, 0.),
        h if h < 120. => (x, chroma, 0.),
        h if h < 180. => (0., chroma, x),
        h if h < 240. => (0., x, chroma),
        h if h < 300. => (x, 0., chroma),
        _ => (chroma, 0., x),
    };
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255 before the cast")]
    let channel = |c: f32| ((c + m) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", channel(r), channel(g), channel(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_hex() {
        for hex in ["#1b4ed8", "#ff0000", "#00ff80", "#808080", "#000000"] {
            assert_eq!(hsv_to_hex(hex_to_hsv(hex)), hex);
        }
        assert_eq!(hex_to_hsv("#ff0000"), [0., 1., 1.]);
    }

    #[test]
    fn colors_canonicalize_to_oklch_like_culori() {
        assert_eq!(canonical("#ffffff").as_deref(), Some("oklch(1 0 0)"));
        assert_eq!(canonical("#000").as_deref(), Some("oklch(0 0 0)"));
        assert_eq!(canonical("oklch(0.62 0.2 280)").as_deref(), Some("oklch(0.62 0.2 280)"));
        assert_eq!(canonical("oklch(62% 0.2 -80 / 50%)").as_deref(), Some("oklch(0.62 0.2 280 / 0.5)"));
        assert_eq!(canonical("rgb(255, 0, 0)"), canonical("#ff0000"));
        assert_eq!(canonical("hsl(0 100% 50%)"), canonical("#f00"));
        assert_eq!(canonical("#ff000080").as_deref().map(|c| c.ends_with("/ 0.502)")), Some(true));
        assert_eq!(canonical("var(--x)"), None);
        assert_eq!(canonical("#12345"), None);
    }

    #[test]
    fn hex_round_trips_through_oklch() {
        for hex in ["#1b4ed8", "#fcfcfc", "#0a0a0a", "#fe9a00", "#346bf1"] {
            assert_eq!(to_hex(&canonical(hex).unwrap()).as_deref(), Some(hex));
        }
        assert_eq!(to_hex("#33669980").as_deref(), Some("#33669980"));
        // An out-of-gamut color keeps its lightness and hue but fits sRGB.
        assert!(to_hex("oklch(0.7 0.5 140)").is_some());
    }

    #[test]
    fn readable_colors_reach_their_contrast() {
        let solved = readable_on("#777777", "#808080", 4.5);
        assert!(contrast(rgb_of(&solved, BLACK), rgb_of("#808080", BLACK)) >= 4.5);
        assert_eq!(readable_on("#000000", "#ffffff", 4.5), "#000000", "already readable");
    }

    #[test]
    fn the_guided_palette_follows_its_two_seeds() {
        let dark = vivid(true, "#101014", "#8b5cf6");
        assert_eq!(to_hex(dark.get("canvas")).as_deref(), Some("#101014"));
        assert_eq!(dark.get("focus"), dark.get("accent"));
        let text = rgb_of(dark.get("text"), BLACK);
        assert!(contrast(text, rgb_of("#101014", BLACK)) >= 7.);
        assert!(is_dark(rgb_of(dark.get("sidebar"), WHITE)));
        let light = vivid(false, "#fafaf7", "#0f766e");
        assert!(!is_dark(rgb_of(light.get("surface"), BLACK)));
        assert!(
            contrast(rgb_of(light.get("messageForeground"), BLACK), rgb_of(light.get("messageSurface"), BLACK)) >= 4.5
        );
    }

    #[test]
    fn a_family_edit_rederives_its_followers() {
        let base = vivid(false, "#ffffff", "#2563eb");
        let edited = update_family(false, &base, "border", "#ff0000");
        let red = canonical("#ff0000").unwrap();
        assert_eq!([edited.get("border"), edited.get("toolbarBorder"), edited.get("sidebarBorder")], [&red[..]; 3]);
        let action = update_family(false, &base, "messageAction", "#000000");
        assert_eq!(to_hex(action.get("messageActionForeground")).as_deref(), Some("#fffaff"));
        // An unparseable value is kept as typed, so the field can show it.
        assert_eq!(update_family(false, &base, "text", "#12").get("text"), "#12");
    }
}
