//! Codex artifact templates (`codexArtifactTemplates.ts` and the `artifact-template` leaf
//! directive of `codexMarkdownDirectives.ts`): a reply's `::artifact-template{…}` line names a
//! template skill, which the chat draws as a card whose Use template drafts a prompt for it.

use std::collections::HashMap;

const DIRECTIVE: &str = "::artifact-template";

/// `CodexArtifactTemplateKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Document,
    Presentation,
    Spreadsheet,
    Site,
    GoogleDocs,
    GoogleSlides,
    GoogleSheets,
    Image,
    Email,
    Slack,
}

impl Kind {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "document" => Self::Document,
            "presentation" => Self::Presentation,
            "spreadsheet" => Self::Spreadsheet,
            "site" => Self::Site,
            "google-docs" => Self::GoogleDocs,
            "google-slides" => Self::GoogleSlides,
            "google-sheets" => Self::GoogleSheets,
            "image" => Self::Image,
            "email" => Self::Email,
            "slack" => Self::Slack,
            _ => return None,
        })
    }

    /// `codexArtifactTemplatePresentationLabel`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Document => "Document template",
            Self::Presentation => "Presentation template",
            Self::Spreadsheet => "Spreadsheet template",
            Self::Site => "Site template",
            Self::GoogleDocs => "Google Doc template",
            Self::GoogleSlides => "Google Slides template",
            Self::GoogleSheets => "Google Sheet template",
            Self::Image => "Image template",
            Self::Email => "Email template",
            Self::Slack => "Slack template",
        }
    }
}

/// `CodexArtifactTemplate`, as the card shows and uses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub kind: Kind,
    pub display_name: String,
    pub skill_name: String,
}

impl Template {
    /// `resolveCodexArtifactTemplate`: every attribute the card needs, valid, or no template.
    fn resolve(attributes: &HashMap<String, String>) -> Option<Self> {
        let kind = Kind::parse(attributes.get("artifact_kind")?)?;
        let display_name = attributes.get("display_name")?.trim();
        let skill_name = attributes.get("skill_name")?;
        let gallery_ok =
            attributes.get("gallery_kind").is_none_or(|kind| matches!(kind.as_str(), "imagegen" | "product-design"));
        let valid = !display_name.is_empty()
            && is_absolute_skill_directory(attributes.get("skill_directory")?)
            && skill_name.starts_with("artifact-template-")
            && gallery_ok;
        valid.then(|| Self { kind, display_name: display_name.to_owned(), skill_name: skill_name.clone() })
    }

    /// What copying the card gives (`renderCodexDirectivesForCopy`): "Name (Kind template)".
    pub fn copy_text(&self) -> String {
        format!("{} ({})", self.display_name, self.kind.label())
    }

    /// `codexArtifactTemplateUsePrompt`.
    pub fn use_prompt(&self) -> String {
        let skill = format!("${}", self.skill_name);
        match self.kind {
            Kind::Document => format!("Create a document using this {skill} about…"),
            Kind::Presentation => format!("Create a presentation using the {skill} template about…"),
            Kind::Spreadsheet => format!("Create a spreadsheet using this {skill} about…"),
            Kind::Site => format!("Create a Site using this {skill} about…"),
            Kind::GoogleDocs => format!("Create a Google Doc using this {skill} about…"),
            Kind::GoogleSlides => format!("Create a Google Slides presentation using this {skill} about…"),
            Kind::GoogleSheets => format!("Create a Google Sheet using this {skill} about…"),
            Kind::Image => format!("Create an image using this {skill} of…"),
            Kind::Email => format!("Draft an email using this {skill} about…"),
            Kind::Slack => format!("Draft a Slack message using this {skill} about…"),
        }
    }

    /// `codexArtifactTemplatePromptToAppend`: the prompt to add to `draft`, or none when the
    /// draft already ends with it as its own word.
    pub fn prompt_to_append(&self, draft: &str) -> Option<String> {
        let prompt = self.use_prompt();
        let trimmed = draft.trim_end();
        let ends_with_prompt = trimmed
            .strip_suffix(prompt.as_str())
            .is_some_and(|before| before.is_empty() || before.chars().next_back().is_some_and(char::is_whitespace));
        (!ends_with_prompt).then_some(prompt)
    }
}

/// `isAbsoluteSkillDirectory`: a POSIX root, a drive path or a UNC share.
fn is_absolute_skill_directory(value: &str) -> bool {
    let bytes = value.as_bytes();
    let posix = value.starts_with('/') && !value.starts_with("//");
    let drive =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    let unc = |separator: char| {
        let doubled = format!("{separator}{separator}");
        value.strip_prefix(doubled.as_str()).is_some_and(|rest| {
            let mut parts = rest.splitn(2, separator);
            let server = parts.next().unwrap_or_default();
            let share = parts.next().unwrap_or_default();
            !server.is_empty() && !share.is_empty() && !share.starts_with(separator)
        })
    };
    posix || drive || unc('\\') || unc('/')
}

/// The template a whole line names, when it is an `::artifact-template` leaf directive that
/// resolves to one.
pub fn parse_line(line: &str) -> Option<Template> {
    let rest = line.trim().strip_prefix(DIRECTIVE)?;
    Template::resolve(&directive_attributes(rest)?)
}

/// The templates of a paragraph whose every line is a template directive, as the card draws them.
pub fn parse_paragraph(source: &str) -> Option<Vec<Template>> {
    if !source.trim_start().starts_with(DIRECTIVE) {
        return None;
    }
    source.lines().filter(|line| !line.trim().is_empty()).map(parse_line).collect()
}

/// `renderCodexDirectivesForCopy` for templates: each template line copies as its card's text.
/// Fenced code keeps its lines.
pub fn render_for_copy(markdown: &str) -> String {
    if !markdown.contains(DIRECTIVE) {
        return markdown.to_owned();
    }
    let mut fence: Option<(char, usize)> = None;
    let mut out = String::with_capacity(markdown.len());
    for line in markdown.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let marker = fence_marker(body);
        match (fence, marker) {
            (None, Some(open)) => fence = Some(open),
            (Some((c, n)), Some((mc, mn))) if c == mc && mn >= n => fence = None,
            _ => {}
        }
        match parse_line(body).filter(|_| fence.is_none() && marker.is_none()) {
            Some(template) => {
                out.push_str(&template.copy_text());
                out.push_str(&line[body.len()..]);
            }
            None => out.push_str(line),
        }
    }
    out
}

/// A fence line's character and length: three or more backticks or tildes, indented up to three.
fn fence_marker(line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let c = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let count = rest.chars().take_while(|&x| x == c).count();
    (indent <= 3 && count >= 3).then_some((c, count))
}

/// The attributes after a leaf directive's name: an optional `[label]`, then an optional
/// `{…}`, then nothing but spaces (`micromark-extension-directive`). `None` when that is not
/// what follows, so the line stays text.
fn directive_attributes(rest: &str) -> Option<HashMap<String, String>> {
    // The name ends at punctuation other than `-` and `_`, or whitespace.
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let mut rest = rest;
    if let Some(label) = rest.strip_prefix('[') {
        rest = skip_label(label)?;
    }
    let mut attributes = HashMap::new();
    if let Some(body) = rest.strip_prefix('{') {
        let end = attribute_list(body, &mut attributes)?;
        rest = &body[end + 1..];
    }
    rest.trim().is_empty().then_some(attributes)
}

/// What follows a `[label]` whose opening bracket is already taken.
fn skip_label(label: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut escaped = false;
    for (index, c) in label.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '[' => depth += 1,
            ']' if depth == 0 => return Some(&label[index + 1..]),
            ']' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Reads `{…}`'s attributes into `attributes`; the byte index of the closing `}`.
fn attribute_list(body: &str, attributes: &mut HashMap<String, String>) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut at = 0;
    loop {
        while bytes.get(at).is_some_and(|b| matches!(b, b' ' | b'\t')) {
            at += 1;
        }
        match *bytes.get(at)? {
            b'}' => return Some(at),
            b'#' | b'.' => {
                // `#id` and `.class` shorthands name nothing a template reads.
                at += 1;
                let start = at;
                while bytes
                    .get(at)
                    .is_some_and(|b| !matches!(b, b' ' | b'\t' | b'}' | b'"' | b'\'' | b'=' | b'<' | b'>' | b'`'))
                {
                    at += 1;
                }
                if at == start {
                    return None;
                }
            }
            first if first.is_ascii_alphabetic() || matches!(first, b':' | b'_') => {
                let start = at;
                while bytes.get(at).is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-'))
                {
                    at += 1;
                }
                let name = &body[start..at];
                let mut value_at = at;
                while bytes.get(value_at).is_some_and(|b| matches!(b, b' ' | b'\t')) {
                    value_at += 1;
                }
                if bytes.get(value_at) != Some(&b'=') {
                    attributes.insert(name.to_owned(), String::new());
                    continue;
                }
                value_at += 1;
                while bytes.get(value_at).is_some_and(|b| matches!(b, b' ' | b'\t')) {
                    value_at += 1;
                }
                let (value, end) = attribute_value(body, value_at)?;
                attributes.insert(name.to_owned(), decode_references(value));
                at = end;
            }
            _ => return None,
        }
    }
}

/// A quoted or unquoted value at `at`, and the index after it.
fn attribute_value(body: &str, at: usize) -> Option<(&str, usize)> {
    let quote = body[at..].chars().next()?;
    if matches!(quote, '"' | '\'') {
        let close = body[at + 1..].find(quote)?;
        return Some((&body[at + 1..at + 1 + close], at + close + 2));
    }
    let len = body[at..].find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '=' | '<' | '>' | '`' | '}'))?;
    (len > 0).then(|| (&body[at..at + len], at + len))
}

/// Character references in an attribute value, as Markdown decodes them.
fn decode_references(value: &str) -> String {
    if !value.contains('&') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .map(|hex| u32::from_str_radix(hex, 16))
                    .or_else(|| name.strip_prefix('#').map(str::parse::<u32>))
                    .and_then(Result::ok)
                    .and_then(char::from_u32),
            }?;
            Some((c, end + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = r#"::artifact-template{skill_name="artifact-template-hello-world" skill_directory="/Users/test/.codex/skills/artifact-template-hello-world" display_name="Hello World" artifact_kind="document"}"#;

    #[test]
    fn a_directive_line_resolves_to_its_template() {
        let template = parse_line(HELLO).expect("a template");
        assert_eq!(template.kind, Kind::Document);
        assert_eq!(template.display_name, "Hello World");
        assert_eq!(template.copy_text(), "Hello World (Document template)");
        assert_eq!(template.use_prompt(), "Create a document using this $artifact-template-hello-world about…");
        let spaced = r#"  ::artifact-template[label]{ artifact_kind='slack' display_name=" Ship it " skill_directory=C:\skills\a skill_name=artifact-template-a #id .card }  "#;
        let template = parse_line(spaced).expect("a template");
        assert_eq!((template.kind, template.display_name.as_str()), (Kind::Slack, "Ship it"));
        let decoded = r#"::artifact-template{artifact_kind="email" display_name="Q&amp;A" skill_directory="//host/share" skill_name="artifact-template-q"}"#;
        assert_eq!(parse_line(decoded).map(|t| t.display_name), Some("Q&A".to_owned()));
    }

    #[test]
    fn invalid_attributes_or_syntax_leave_the_line_as_text() {
        for line in [
            // Unknown kind, relative directory, foreign skill, blank name, bad gallery.
            r#"::artifact-template{artifact_kind="video" display_name="A" skill_directory="/s" skill_name="artifact-template-a"}"#,
            r#"::artifact-template{artifact_kind="site" display_name="A" skill_directory="skills/a" skill_name="artifact-template-a"}"#,
            r#"::artifact-template{artifact_kind="site" display_name="A" skill_directory="//s" skill_name="artifact-template-a"}"#,
            r#"::artifact-template{artifact_kind="site" display_name="A" skill_directory="/s" skill_name="my-skill"}"#,
            r#"::artifact-template{artifact_kind="site" display_name="  " skill_directory="/s" skill_name="artifact-template-a"}"#,
            r#"::artifact-template{artifact_kind="site" display_name="A" skill_directory="/s" skill_name="artifact-template-a" gallery_kind="x"}"#,
            // Another directive name, an unclosed list, text after it.
            r#"::artifact-templates{artifact_kind="site" display_name="A" skill_directory="/s" skill_name="artifact-template-a"}"#,
            r#"::artifact-template{artifact_kind="site" display_name="A" skill_directory="/s" skill_name="artifact-template-a""#,
            r#"::artifact-template{artifact_kind="site" display_name="A" skill_directory="/s" skill_name="artifact-template-a"} more"#,
        ] {
            assert_eq!(parse_line(line), None, "{line}");
        }
        let gallery = r#"::artifact-template{artifact_kind="image" display_name="A" skill_directory="/s" skill_name="artifact-template-a" gallery_kind="imagegen"}"#;
        assert!(parse_line(gallery).is_some());
    }

    #[test]
    fn a_paragraph_is_cards_only_when_every_line_is_a_template() {
        assert_eq!(parse_paragraph(&format!("{HELLO}\n{HELLO}")).map(|t| t.len()), Some(2));
        assert_eq!(parse_paragraph(&format!("{HELLO}\nand text")), None);
        assert_eq!(parse_paragraph("plain text"), None);
    }

    #[test]
    fn copying_a_reply_writes_each_template_as_its_card_text() {
        let reply = format!("Made one.\n\n{HELLO}\n\n```\n{HELLO}\n```\n");
        assert_eq!(
            render_for_copy(&reply),
            format!("Made one.\n\nHello World (Document template)\n\n```\n{HELLO}\n```\n")
        );
    }

    #[test]
    fn using_a_template_appends_its_prompt_once() {
        let template = parse_line(HELLO).unwrap();
        let prompt = template.use_prompt();
        assert_eq!(template.prompt_to_append(""), Some(prompt.clone()));
        assert_eq!(template.prompt_to_append("Hi"), Some(prompt.clone()));
        assert_eq!(template.prompt_to_append(&format!("Hi {prompt}  ")), None);
        assert_eq!(template.prompt_to_append(&prompt), None);
        assert_eq!(template.prompt_to_append(&format!("x{prompt}")), Some(prompt));
    }
}
