//! TeX to Unicode for chat math. The web typesets with KaTeX; natively there is no TeX
//! engine, so formulas read as their Unicode form: Greek and operator symbols, scripts as
//! superscript and subscript characters where Unicode has them, fractions and roots inline.

const SYMBOLS: &[(&str, &str)] = &[
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("pi", "π"),
    ("rho", "ρ"),
    ("sigma", "σ"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "φ"),
    ("varphi", "φ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Upsilon", "Υ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    ("times", "×"),
    ("cdot", "·"),
    ("div", "÷"),
    ("pm", "±"),
    ("mp", "∓"),
    ("leq", "≤"),
    ("le", "≤"),
    ("geq", "≥"),
    ("ge", "≥"),
    ("neq", "≠"),
    ("ne", "≠"),
    ("approx", "≈"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("equiv", "≡"),
    ("propto", "∝"),
    ("infty", "∞"),
    ("sum", "∑"),
    ("prod", "∏"),
    ("int", "∫"),
    ("oint", "∮"),
    ("partial", "∂"),
    ("nabla", "∇"),
    ("to", "→"),
    ("rightarrow", "→"),
    ("leftarrow", "←"),
    ("leftrightarrow", "↔"),
    ("Rightarrow", "⇒"),
    ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"),
    ("implies", "⇒"),
    ("iff", "⇔"),
    ("mapsto", "↦"),
    ("in", "∈"),
    ("notin", "∉"),
    ("ni", "∋"),
    ("subset", "⊂"),
    ("subseteq", "⊆"),
    ("supset", "⊃"),
    ("supseteq", "⊇"),
    ("cup", "∪"),
    ("cap", "∩"),
    ("emptyset", "∅"),
    ("varnothing", "∅"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("neg", "¬"),
    ("lnot", "¬"),
    ("land", "∧"),
    ("wedge", "∧"),
    ("lor", "∨"),
    ("vee", "∨"),
    ("oplus", "⊕"),
    ("otimes", "⊗"),
    ("circ", "∘"),
    ("bullet", "•"),
    ("star", "⋆"),
    ("ldots", "…"),
    ("dots", "…"),
    ("cdots", "⋯"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
    ("prime", "′"),
    ("degree", "°"),
    ("angle", "∠"),
    ("perp", "⊥"),
    ("parallel", "∥"),
    ("mid", "∣"),
    ("ell", "ℓ"),
    ("hbar", "ℏ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("aleph", "ℵ"),
    ("langle", "⟨"),
    ("rangle", "⟩"),
    ("lfloor", "⌊"),
    ("rfloor", "⌋"),
    ("lceil", "⌈"),
    ("rceil", "⌉"),
    ("ll", "≪"),
    ("gg", "≫"),
    ("quad", "  "),
    ("qquad", "    "),
    ("sqrt", "√"),
    ("log", "log"),
    ("ln", "ln"),
    ("exp", "exp"),
    ("sin", "sin"),
    ("cos", "cos"),
    ("tan", "tan"),
    ("lim", "lim"),
    ("max", "max"),
    ("min", "min"),
    ("det", "det"),
    ("mathbb{R}", "ℝ"),
    ("mathbb{N}", "ℕ"),
    ("mathbb{Z}", "ℤ"),
    ("mathbb{Q}", "ℚ"),
    ("mathbb{C}", "ℂ"),
    ("{", "{"),
    ("}", "}"),
    ("%", "%"),
    ("$", "$"),
    ("&", "&"),
    ("_", "_"),
    ("#", "#"),
    (",", " "),
    (";", " "),
    (":", " "),
    ("!", ""),
    (" ", " "),
    ("\\", "\n"),
];

const SUPERSCRIPTS: &[(char, char)] = &[
    ('0', '⁰'),
    ('1', '¹'),
    ('2', '²'),
    ('3', '³'),
    ('4', '⁴'),
    ('5', '⁵'),
    ('6', '⁶'),
    ('7', '⁷'),
    ('8', '⁸'),
    ('9', '⁹'),
    ('+', '⁺'),
    ('-', '⁻'),
    ('=', '⁼'),
    ('(', '⁽'),
    (')', '⁾'),
    ('n', 'ⁿ'),
    ('i', 'ⁱ'),
    ('T', 'ᵀ'),
    ('a', 'ᵃ'),
    ('b', 'ᵇ'),
    ('c', 'ᶜ'),
    ('d', 'ᵈ'),
    ('e', 'ᵉ'),
    ('k', 'ᵏ'),
    ('m', 'ᵐ'),
    ('x', 'ˣ'),
    ('y', 'ʸ'),
    ('′', '′'),
];

const SUBSCRIPTS: &[(char, char)] = &[
    ('0', '₀'),
    ('1', '₁'),
    ('2', '₂'),
    ('3', '₃'),
    ('4', '₄'),
    ('5', '₅'),
    ('6', '₆'),
    ('7', '₇'),
    ('8', '₈'),
    ('9', '₉'),
    ('+', '₊'),
    ('-', '₋'),
    ('=', '₌'),
    ('(', '₍'),
    (')', '₎'),
    ('a', 'ₐ'),
    ('e', 'ₑ'),
    ('i', 'ᵢ'),
    ('j', 'ⱼ'),
    ('k', 'ₖ'),
    ('n', 'ₙ'),
    ('o', 'ₒ'),
    ('x', 'ₓ'),
    ('t', 'ₜ'),
    ('m', 'ₘ'),
    ('p', 'ₚ'),
    ('s', 'ₛ'),
];

/// The group at the start of `rest`: `{...}` with its braces matched, a command, or one character.
fn group(rest: &str) -> (&str, &str) {
    if let Some(inner) = rest.strip_prefix('{') {
        let mut depth = 1;
        for (i, c) in inner.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return (&inner[..i], &inner[i + 1..]);
                    }
                }
                _ => {}
            }
        }
        return (inner, "");
    }
    if let Some(command) = rest.strip_prefix('\\') {
        let len = command.chars().take_while(char::is_ascii_alphabetic).map(char::len_utf8).sum::<usize>().max(1);
        let end = (1 + len).min(rest.len());
        return (&rest[..end], &rest[end..]);
    }
    let len = rest.chars().next().map_or(0, char::len_utf8);
    (&rest[..len], &rest[len..])
}

/// `body` as script characters, or `None` when one has no script form.
fn script(body: &str, table: &[(char, char)]) -> Option<String> {
    body.chars().map(|c| table.iter().find(|(from, _)| *from == c).map(|(_, to)| *to)).collect()
}

/// A formula's TeX as Unicode text. Like TeX, it keeps no runs of spaces.
pub fn to_unicode(tex: &str) -> String {
    let text = convert(tex);
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if !(c == ' ' && out.ends_with(' ')) {
            out.push(c);
        }
    }
    out
}

fn convert(tex: &str) -> String {
    let mut out = String::with_capacity(tex.len());
    let mut rest = tex.trim();
    while let Some(c) = rest.chars().next() {
        match c {
            '^' | '_' => {
                let (body, after) = group(&rest[1..]);
                let body = convert(body);
                let table = if c == '^' { SUPERSCRIPTS } else { SUBSCRIPTS };
                match script(&body, table) {
                    Some(scripted) => out.push_str(&scripted),
                    None if body.chars().count() == 1 => {
                        out.push(c);
                        out.push_str(&body);
                    }
                    None => out.push_str(&format!("{c}({body})")),
                }
                rest = after;
            }
            '{' | '}' => rest = &rest[1..],
            '\\' => {
                let command: String = rest[1..].chars().take_while(char::is_ascii_alphabetic).collect();
                let after = &rest[1 + command.len()..];
                match command.as_str() {
                    "frac" | "dfrac" | "tfrac" => {
                        let (top, after) = group(after.trim_start());
                        let (bottom, after) = group(after.trim_start());
                        let (top, bottom) = (convert(top), convert(bottom));
                        let wrap = |s: String| if s.chars().count() > 1 { format!("({s})") } else { s };
                        out.push_str(&format!("{}/{}", wrap(top), wrap(bottom)));
                        rest = after;
                    }
                    "sqrt" => {
                        let (body, after) = group(after.trim_start());
                        let body = convert(body);
                        out.push('√');
                        out.push_str(&if body.chars().count() > 1 { format!("({body})") } else { body });
                        rest = after;
                    }
                    "text" | "mathrm" | "mathbf" | "mathit" | "mathsf" | "mathtt" | "operatorname" | "textbf"
                    | "boldsymbol" => {
                        let (body, after) = group(after.trim_start());
                        out.push_str(&if command.starts_with("text") { body.to_owned() } else { convert(body) });
                        rest = after;
                    }
                    "mathbb" => {
                        let (body, after) = group(after.trim_start());
                        let key = format!("mathbb{{{body}}}");
                        out.push_str(SYMBOLS.iter().find(|(k, _)| *k == key).map_or(body, |(_, v)| v));
                        rest = after;
                    }
                    "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "displaystyle" | "limits" => rest = after,
                    "" => {
                        let (symbol, after) = group(rest);
                        let key = &symbol[1..];
                        out.push_str(SYMBOLS.iter().find(|(k, _)| *k == key).map_or(key, |(_, v)| v));
                        rest = after;
                    }
                    name => {
                        out.push_str(SYMBOLS.iter().find(|(k, _)| *k == name).map_or(name, |(_, v)| v));
                        rest = after;
                    }
                }
            }
            _ => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::to_unicode;

    #[test]
    fn formulas_read_as_unicode() {
        assert_eq!(to_unicode("E = mc^2"), "E = mc²");
        assert_eq!(to_unicode("x_{i+1} \\leq \\alpha"), "xᵢ₊₁ ≤ α");
        assert_eq!(to_unicode("\\frac{a+b}{2}"), "(a+b)/2");
        assert_eq!(to_unicode("\\sqrt{x^2 + y^2}"), "√(x² + y²)");
        assert_eq!(to_unicode("\\sum_{n=1}^{\\infty} \\frac{1}{n^2}"), "∑ₙ₌₁^∞ 1/(n²)");
        assert_eq!(to_unicode("\\mathbb{R}^n \\text{ is real}"), "ℝⁿ is real");
        assert_eq!(to_unicode("a^{bq}"), "a^(bq)");
    }
}
