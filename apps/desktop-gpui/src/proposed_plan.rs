//! Proposed plan text (`proposedPlanText.ts`, `proposedPlan.ts`): the card's title,
//! displayed body and collapsed preview, and the markdown it exports.

/// A `#`..`######` heading's text: up to three spaces, the hashes, then whitespace.
fn heading(line: &str) -> Option<&str> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let body = &rest[hashes..];
    let text = body.trim_start();
    (text.len() < body.len() && !text.is_empty()).then_some(text)
}

/// `proposedPlanTitle`.
pub fn title(markdown: &str) -> Option<&str> {
    markdown.lines().find_map(heading).map(str::trim).filter(|t| !t.is_empty())
}

/// `stripDisplayedPlanMarkdown`: drops the title line and a leading "Summary" heading.
pub fn displayed(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.trim_end().lines().collect();
    let blank = |i: usize| lines.get(i).is_some_and(|l| l.trim().is_empty());
    let mut start = usize::from(lines.first().is_some_and(|l| heading(l).is_some()));
    while blank(start) {
        start += 1;
    }
    if lines.get(start).and_then(|l| heading(l)).is_some_and(|h| h.trim().eq_ignore_ascii_case("summary")) {
        start += 1;
        while blank(start) {
            start += 1;
        }
    }
    lines.get(start..).unwrap_or_default().join("\n")
}

/// `canCollapse` in `ProposedPlanCard.tsx`.
pub fn can_collapse(markdown: &str) -> bool {
    markdown.len() > 900 || markdown.split('\n').count() > 20
}

/// `buildCollapsedProposedPlanPreviewMarkdown`: the first `max_lines` non-blank lines.
pub fn collapsed_preview(markdown: &str, max_lines: usize) -> String {
    let displayed = displayed(markdown);
    let mut preview: Vec<&str> = Vec::new();
    let mut visible = 0;
    let mut more = false;
    for line in displayed.trim_end().lines().map(str::trim_end) {
        let is_visible = !line.trim().is_empty();
        if is_visible && visible >= max_lines {
            more = true;
            break;
        }
        preview.push(line);
        visible += usize::from(is_visible);
    }
    while preview.last().is_some_and(|l| l.trim().is_empty()) {
        preview.pop();
    }
    if preview.is_empty() {
        return title(markdown).unwrap_or("Plan preview unavailable.").to_owned();
    }
    if more {
        preview.extend(["", "..."]);
    }
    preview.join("\n")
}

/// `buildProposedPlanMarkdownFilename`.
pub fn filename(markdown: &str) -> String {
    let lowered = title(markdown).unwrap_or("plan").to_lowercase();
    let mut slug = String::new();
    for c in lowered.chars().filter(|c| !"`'\".,!?()[]{}".contains(*c)) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    format!("{}.md", if slug.is_empty() { "plan" } else { slug })
}

/// `normalizePlanMarkdownForExport`.
pub fn export(markdown: &str) -> String {
    format!("{}\n", markdown.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_bodies_skip_the_heading_and_a_summary_heading() {
        let plan = "# Ship the stash\n\n## Summary\n\nDo it.\n";
        assert_eq!(title(plan), Some("Ship the stash"));
        assert_eq!(displayed(plan), "Do it.");
        assert_eq!(title("intro\n   ### Later  \n"), Some("Later"));
        assert_eq!(title("    # code, not a heading\n#nospace"), None);
        assert_eq!(displayed("No heading\nline"), "No heading\nline");
    }

    #[test]
    fn previews_keep_ten_visible_lines() {
        let body: Vec<String> = (1..=12).map(|i| format!("- step {i}")).collect();
        let plan = format!("# Plan\n\n{}", body.join("\n\n"));
        let preview = collapsed_preview(&plan, 10);
        assert!(preview.starts_with("- step 1\n\n- step 2"));
        assert!(preview.ends_with("- step 10\n\n..."));
        assert_eq!(collapsed_preview("# Only a title", 10), "Only a title");
        assert_eq!(collapsed_preview("", 10), "Plan preview unavailable.");
        assert!(can_collapse(&plan) && !can_collapse("# Short"));
    }

    #[test]
    fn filenames_slug_the_title() {
        assert_eq!(filename("# Fix the `login` flow (v2)!\n"), "fix-the-login-flow-v2.md");
        assert_eq!(filename("no title"), "plan.md");
        assert_eq!(filename("# ¿?"), "plan.md");
        assert_eq!(export("# A\n\n\n"), "# A\n");
    }
}
