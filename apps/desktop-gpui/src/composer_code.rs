//! A composer code block's language (`composer-code-languages.ts`): the languages its picker
//! offers, the aliases an info string may use, and the icon file name each one takes
//! (`syntheticFileNameForLanguageId` in `pierre-icons.ts`).

/// `CODE_BLOCK_LANGUAGES`: id and label; the empty id is plain text.
pub const LANGUAGES: [(&str, &str); 40] = [
    ("", "Plain text"),
    ("asm", "Assembly"),
    ("bash", "Bash"),
    ("c", "C"),
    ("cpp", "C++"),
    ("csharp", "C#"),
    ("css", "CSS"),
    ("dart", "Dart"),
    ("diff", "Diff"),
    ("dockerfile", "Dockerfile"),
    ("elixir", "Elixir"),
    ("go", "Go"),
    ("graphql", "GraphQL"),
    ("haskell", "Haskell"),
    ("html", "HTML"),
    ("java", "Java"),
    ("javascript", "JavaScript"),
    ("json", "JSON"),
    ("jsx", "JSX"),
    ("kotlin", "Kotlin"),
    ("lua", "Lua"),
    ("markdown", "Markdown"),
    ("php", "PHP"),
    ("powershell", "PowerShell"),
    ("python", "Python"),
    ("r", "R"),
    ("ruby", "Ruby"),
    ("rust", "Rust"),
    ("scala", "Scala"),
    ("scss", "SCSS"),
    ("sql", "SQL"),
    ("svelte", "Svelte"),
    ("swift", "Swift"),
    ("toml", "TOML"),
    ("tsx", "TSX"),
    ("typescript", "TypeScript"),
    ("vue", "Vue"),
    ("xml", "XML"),
    ("yaml", "YAML"),
    ("zig", "Zig"),
];

/// `LANGUAGE_ALIASES`.
const ALIASES: [(&str, &str); 16] = [
    ("assembly", "asm"),
    ("cs", "csharp"),
    ("c++", "cpp"),
    ("js", "javascript"),
    ("md", "markdown"),
    ("plaintext", ""),
    ("py", "python"),
    ("rb", "ruby"),
    ("rs", "rust"),
    ("s", "asm"),
    ("sh", "bash"),
    ("shell", "bash"),
    ("text", ""),
    ("ts", "typescript"),
    ("txt", ""),
    ("yml", "yaml"),
];

/// `codeLanguageEntry`: the listed language an info string's language names, through aliases.
pub fn entry(language: &str) -> Option<(&'static str, &'static str)> {
    let lower = language.to_lowercase();
    let id = ALIASES.iter().find(|(alias, _)| *alias == lower).map_or(lower.as_str(), |(_, id)| id);
    LANGUAGES.iter().find(|(entry, _)| *entry == id).copied()
}

/// `codeLanguageLabel`: the listed label, else the language as written.
pub fn label(language: &str) -> String {
    entry(language).map_or_else(|| language.to_owned(), |(_, label)| label.to_owned())
}

/// The picker's rows: the block's own language first when it is not listed, then the list,
/// filtered by label or id (`ComposerCodeBlockLanguagePicker`).
pub fn choices(current: &str, query: &str) -> Vec<(String, String)> {
    let mut items: Vec<(String, String)> = Vec::with_capacity(LANGUAGES.len() + 1);
    if !LANGUAGES.iter().any(|(id, _)| *id == current) {
        items.push((current.to_owned(), current.to_owned()));
    }
    items.extend(LANGUAGES.iter().map(|(id, label)| ((*id).to_owned(), (*label).to_owned())));
    let needle = query.trim().to_lowercase();
    items.retain(|(id, label)| needle.is_empty() || label.to_lowercase().contains(&needle) || id.contains(&needle));
    items
}

/// The picker's selected id for a block's language: its listed id, else the language itself.
pub fn current(language: &str) -> String {
    entry(language).map_or_else(|| language.to_owned(), |(id, _)| id.to_owned())
}

/// `withInfoStringLanguage`: the info string with its language replaced. Plain text clears it
/// unless trailing metadata would then be read as the language.
pub fn with_language(info: &str, language: &str) -> String {
    let leading = &info[..info.len() - info.trim_start().len()];
    let current = crate::composer_markdown::language_of_info(info);
    let rest = &info[leading.len() + current.len()..];
    if !language.is_empty() {
        format!("{leading}{language}{rest}")
    } else if rest.trim().is_empty() {
        String::new()
    } else {
        format!("{leading}text{rest}")
    }
}

/// `syntheticFileNameForLanguageId`: a file name whose icon stands for the language.
pub fn file_name(language: &str) -> String {
    let normalized = language.to_lowercase();
    if normalized == "dockerfile" {
        return "Dockerfile".to_owned();
    }
    let extension = match normalized.as_str() {
        "" => "txt",
        "bash" | "shell" | "shellscript" => "sh",
        "csharp" => "cs",
        "elixir" => "ex",
        "haskell" => "hs",
        "javascript" => "js",
        "kotlin" => "kt",
        "markdown" => "md",
        "plaintext" => "txt",
        "powershell" => "ps1",
        "python" => "py",
        "ruby" => "rb",
        "rust" => "rs",
        "typescript" => "ts",
        "yaml" => "yml",
        other => other,
    };
    format!("file.{extension}")
}

/// The grammar name the highlighter knows a language by, or `None` for plain text.
pub fn grammar(language: &str) -> Option<String> {
    let id = current(language);
    match id.as_str() {
        "" => None,
        "jsx" => Some("javascript".to_owned()),
        _ => Some(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_listed_languages() {
        assert_eq!(label("rs"), "Rust");
        assert_eq!(label("TS"), "TypeScript");
        assert_eq!(label("txt"), "Plain text");
        assert_eq!(label("brainfuck"), "brainfuck");
        assert_eq!(current("py"), "python");
        assert_eq!(current("brainfuck"), "brainfuck");
        assert_eq!(grammar("text"), None);
        assert_eq!(grammar("sh").as_deref(), Some("bash"));
    }

    #[test]
    fn replacing_the_language_keeps_the_info_strings_metadata() {
        assert_eq!(with_language("rust", "python"), "python");
        assert_eq!(with_language(" rust title=\"a\"", "go"), " go title=\"a\"");
        assert_eq!(with_language("", "go"), "go");
        // Plain text drops the language, unless metadata would then read as one.
        assert_eq!(with_language("rust", ""), "");
        assert_eq!(with_language("rust title", ""), "text title");
    }

    #[test]
    fn the_picker_lists_an_unlisted_language_first_and_filters_by_label_or_id() {
        let all = choices("brainfuck", "");
        assert_eq!(all[0], ("brainfuck".to_owned(), "brainfuck".to_owned()));
        assert_eq!(all.len(), LANGUAGES.len() + 1);
        let found: Vec<String> = choices("rust", "SCR").into_iter().map(|(id, _)| id).collect();
        assert_eq!(found, ["javascript", "typescript"]);
        assert_eq!(choices("", "c#")[0].0, "csharp");
        assert!(choices("", "nothing like it").is_empty());
    }

    #[test]
    fn languages_take_their_icon_file_names() {
        assert_eq!(file_name("rust"), "file.rs");
        assert_eq!(file_name("Dockerfile"), "Dockerfile");
        assert_eq!(file_name("go"), "file.go");
        assert_eq!(file_name(""), "file.txt");
    }
}
