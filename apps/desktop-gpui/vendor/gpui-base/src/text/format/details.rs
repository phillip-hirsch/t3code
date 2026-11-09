//! `<details>` disclosures that span Markdown blocks. CommonMark ends an HTML
//! block at a blank line, so `<details><summary>…</summary>`, the Markdown
//! inside it and `</details>` arrive as sibling blocks. This pairs the tags
//! across siblings, as re-parsing the tree with rehype-raw does, so the
//! Markdown between them becomes the disclosure's content. A disclosure that
//! is never closed holds everything after it, as an HTML parser closes it at
//! the end of the document (and as a streamed reply reads until it arrives).

use std::ops::Range;
use std::sync::Arc;

use super::html;
use crate::text::node::{BlockNode, NodeContext, Paragraph, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    Open,
    Close,
}

/// The `<details …>` and `</details>` tags in `html`, by byte range. A tag
/// still being streamed (no `>` yet) is not one.
fn details_tags(html: &str) -> Vec<(Range<usize>, Tag)> {
    let lower = html.to_ascii_lowercase();
    let mut tags = Vec::new();
    let mut from = 0;
    while let Some(found) = lower[from..].find('<') {
        let at = from + found;
        let rest = &lower[at..];
        let (tag, name_len) = if rest.starts_with("</details") {
            (Tag::Close, "</details".len())
        } else if rest.starts_with("<details") {
            (Tag::Open, "<details".len())
        } else {
            from = at + 1;
            continue;
        };
        let ends_name = rest
            .as_bytes()
            .get(name_len)
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'));
        if !ends_name {
            from = at + 1;
            continue;
        }
        let Some(end) = rest.find('>') else {
            break;
        };
        tags.push((at..at + end + 1, tag));
        from = at + end + 1;
    }
    tags
}

struct Frame {
    open: bool,
    start: usize,
    summary: Option<Paragraph>,
    children: Vec<BlockNode>,
}

impl Frame {
    fn finish(self, end: usize) -> BlockNode {
        BlockNode::Details {
            summary: self.summary.unwrap_or_else(Paragraph::details_fallback),
            children: self.children,
            open: self.open,
            span: Some(Span {
                start: self.start,
                end: end.max(self.start),
            }),
        }
    }
}

/// Collects one container's blocks, nesting those between a `<details>` and
/// its `</details>` inside the disclosure.
#[derive(Default)]
pub(super) struct DetailsGrouper {
    stack: Vec<Frame>,
    blocks: Vec<BlockNode>,
}

impl DetailsGrouper {
    /// Adds a converted block to the open disclosure, or the container.
    pub(super) fn push(&mut self, block: BlockNode) {
        match self.stack.last_mut() {
            Some(frame) => frame.children.push(block),
            None => self.blocks.push(block),
        }
    }

    /// Takes in an HTML block at `start` in the document when it opens or
    /// closes a disclosure; `false` leaves it to the ordinary conversion.
    pub(super) fn html(&mut self, value: &str, start: usize, cx: &mut NodeContext) -> bool {
        let tags = details_tags(value);
        if tags.is_empty() {
            // A summary after a blank line is still the open disclosure's.
            let labels = self.stack.last().is_some_and(|frame| frame.summary.is_none())
                && value.to_ascii_lowercase().contains("<summary");
            if labels {
                self.fragment(value, start, cx);
            }
            return labels;
        }
        let mut cursor = 0;
        for (range, tag) in tags {
            self.fragment(&value[cursor..range.start], start + cursor, cx);
            match tag {
                Tag::Open => {
                    let open = html::parse_details(&value[range.clone()], cx)
                        .is_some_and(|(open, ..)| open);
                    self.stack.push(Frame {
                        open,
                        start: start + range.start,
                        summary: None,
                        children: Vec::new(),
                    });
                }
                // A stray close tag closes nothing, as in HTML.
                Tag::Close => {
                    if let Some(frame) = self.stack.pop() {
                        let block = frame.finish(start + range.end);
                        self.push(block);
                    }
                }
            }
            cursor = range.end;
        }
        self.fragment(&value[cursor..], start + cursor, cx);
        true
    }

    /// HTML between the tags: inside a disclosure, its summary (the first
    /// one) and content; outside, ordinary HTML blocks.
    fn fragment(&mut self, fragment: &str, start: usize, cx: &mut NodeContext) {
        if fragment.trim().is_empty() {
            return;
        }
        let span = Span {
            start,
            end: start + fragment.len(),
        };
        let Some(frame) = self.stack.last_mut() else {
            if let Ok(document) = html::parse(fragment, cx) {
                self.blocks.push(BlockNode::Root {
                    children: Arc::unwrap_or_clone(document.blocks),
                    span: Some(span),
                });
            }
            return;
        };
        let Some((_, summary, children)) = html::parse_details(&format!("<details>{fragment}"), cx)
        else {
            return;
        };
        if let Some(mut summary) = summary {
            if frame.summary.is_none() {
                summary.span = Some(summary_span(fragment, span));
                frame.summary = Some(summary);
            } else {
                frame.children.push(BlockNode::Paragraph(summary));
            }
        }
        frame.children.extend(children);
    }

    /// The container's blocks; a disclosure still open ends at `end`.
    pub(super) fn finish(mut self, end: usize) -> Vec<BlockNode> {
        while let Some(frame) = self.stack.pop() {
            let block = frame.finish(end);
            self.push(block);
        }
        self.blocks
    }
}

/// Where a summary's source sits in `fragment`, at `span`: from its tag to
/// its end tag. Its start keys the summary's text for highlights.
fn summary_span(fragment: &str, span: Span) -> Span {
    let lower = fragment.to_ascii_lowercase();
    let start = lower.find("<summary").unwrap_or(0);
    let end = lower
        .find("</summary>")
        .map_or(fragment.len(), |end| end + "</summary>".len());
    Span {
        start: span.start + start,
        end: span.start + end.max(start),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_found_whole_and_case_blind() {
        let html = "<DETAILS open>\n<summary>x</summary></details><detailsx><details";
        assert_eq!(
            details_tags(html),
            [(0..14, Tag::Open), (35..45, Tag::Close)]
        );
    }
}
