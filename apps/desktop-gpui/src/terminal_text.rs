//! A command's output as a terminal shows it, for expanded command rows: a
//! colorless port of `appendTerminalOutput` (`packages/shared/src/terminalOutput.ts`).
//! Carriage returns, cursor moves, saves and erases apply, so progress bars
//! fold in place; colors and other escapes are dropped.

use gpui_kit::SharedString;

/// `TERMINAL_OUTPUT_TAIL_CHARS`: older output is dropped and flagged.
const TAIL_CHARS: usize = 64 * 1024;
/// `LIVE_LINES`: lines further up are settled and cursor moves cannot reach them.
const LIVE_LINES: usize = 64;

#[derive(Clone, Debug, Default, PartialEq)]
enum Parse {
    #[default]
    Text,
    Escape,
    Csi(String),
    /// An OSC or other string sequence, ended by BEL or `ESC \`.
    Osc {
        escape: bool,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalText {
    lines: Vec<Vec<char>>,
    /// [`Self::text_content`], rebuilt once per append so rendering only clones it.
    folded: SharedString,
    row: usize,
    col: usize,
    saved: Option<(usize, usize)>,
    parse: Parse,
    /// Earlier output was dropped, here or by the server.
    pub truncated: bool,
}

impl TerminalText {
    pub fn append(&mut self, text: &str) {
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
        for ch in text.chars() {
            match std::mem::take(&mut self.parse) {
                Parse::Text => self.text(ch),
                Parse::Escape => match ch {
                    '[' => self.parse = Parse::Csi(String::new()),
                    ']' | 'P' | '_' | '^' | 'X' => self.parse = Parse::Osc { escape: false },
                    '7' => self.saved = Some((self.row, self.col)),
                    '8' => self.restore(),
                    _ => {}
                },
                Parse::Csi(mut params) => {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        self.csi(&params, ch);
                    } else {
                        params.push(ch);
                        self.parse = Parse::Csi(params);
                    }
                }
                Parse::Osc { escape } => match ch {
                    '\u{7}' => {}
                    '\\' if escape => {}
                    _ => self.parse = Parse::Osc { escape: ch == '\u{1b}' },
                },
            }
        }
        self.trim();
        self.folded = self.fold().into();
    }

    /// Starts over, keeping [`Self::truncated`].
    pub fn clear(&mut self) {
        *self = Self { truncated: self.truncated, ..Self::default() };
    }

    fn text(&mut self, ch: char) {
        match ch {
            '\u{1b}' => self.parse = Parse::Escape,
            '\n' => {
                self.row += 1;
                self.col = 0;
                if self.row == self.lines.len() {
                    self.lines.push(Vec::new());
                }
            }
            '\r' => self.col = 0,
            '\u{8}' => self.col = self.col.saturating_sub(1),
            ch if ch.is_control() && ch != '\t' => {}
            ch => {
                let line = &mut self.lines[self.row];
                if self.col < line.len() {
                    line[self.col] = ch;
                } else {
                    line.resize(self.col, ' ');
                    line.push(ch);
                }
                self.col += 1;
            }
        }
    }

    fn csi(&mut self, params: &str, command: char) {
        let n = params.trim_start_matches('?').split(';').next().and_then(|n| n.parse::<usize>().ok());
        let count = n.unwrap_or(1).max(1);
        let top = self.lines.len().saturating_sub(LIVE_LINES);
        match command {
            'A' => self.row = self.row.saturating_sub(count).max(top),
            // `moveTo`: rows stay on existing lines, columns within the tail budget.
            'B' => self.row = self.row.saturating_add(count).min(self.lines.len() - 1),
            'C' => self.col = self.col.saturating_add(count).min(TAIL_CHARS),
            'D' => self.col = self.col.saturating_sub(count),
            'G' => self.col = (count - 1).min(TAIL_CHARS),
            'K' => {
                let line = &mut self.lines[self.row];
                match n.unwrap_or(0) {
                    0 => line.truncate(self.col),
                    1 => line.iter_mut().take(self.col + 1).for_each(|c| *c = ' '),
                    _ => line.clear(),
                }
            }
            'J' if matches!(n, Some(2 | 3)) => {
                self.lines = vec![Vec::new()];
                (self.row, self.col) = (0, 0);
            }
            's' => self.saved = Some((self.row, self.col)),
            'u' => self.restore(),
            _ => {}
        }
    }

    fn restore(&mut self) {
        if let Some((row, col)) = self.saved {
            self.row = row.min(self.lines.len() - 1);
            self.col = col;
        }
    }

    /// Drops whole lines from the top past the tail budget.
    fn trim(&mut self) {
        let mut total: usize = self.lines.iter().map(|line| line.len() + 1).sum();
        let mut drop = 0;
        while total > TAIL_CHARS && drop + 1 < self.lines.len() {
            total -= self.lines[drop].len() + 1;
            drop += 1;
        }
        if drop > 0 {
            self.lines.drain(..drop);
            self.row = self.row.saturating_sub(drop);
            self.saved = self.saved.map(|(row, col)| (row.saturating_sub(drop), col));
            self.truncated = true;
        }
    }

    /// The folded text, without trailing blank lines.
    pub fn text_content(&self) -> SharedString {
        self.folded.clone()
    }

    fn fold(&self) -> String {
        let end = self.lines.iter().rposition(|line| !line.is_empty()).map_or(0, |i| i + 1);
        let mut out = String::new();
        for (index, line) in self.lines[..end].iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.extend(line.iter().collect::<String>().trim_end().chars());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(chunks: &[&str]) -> String {
        let mut text = TerminalText::default();
        for chunk in chunks {
            text.append(chunk);
        }
        text.text_content().to_string()
    }

    #[test]
    fn carriage_returns_and_erases_fold_progress_in_place() {
        assert_eq!(fold(&["10%\r50%\r100%\n", "done\n"]), "100%\ndone");
        assert_eq!(fold(&["downloading...\r\x1b[Kok"]), "ok");
        assert_eq!(fold(&["a\nb\nc\x1b[2A\x1b[1GX"]), "X\nb\nc");
    }

    #[test]
    fn colors_and_osc_titles_are_dropped_even_split_across_chunks() {
        assert_eq!(fold(&["\x1b[3", "2mgreen\x1b[0m plain"]), "green plain");
        assert_eq!(fold(&["\x1b]0;title\x07after"]), "after");
        assert_eq!(fold(&["\x1b]8;;http://x\x1b\\link"]), "link");
    }

    #[test]
    fn saved_cursor_restores_and_moves_stay_in_the_live_lines() {
        assert_eq!(fold(&["one\x1b7\ntwo\x1b8!"]), "one!\ntwo");
        let many: String = (0..100).map(|i| format!("{i}\n")).collect();
        // 99 lines up stops at the first of the 64 live lines.
        assert_eq!(fold(&[&many, "\x1b[99AX"]).lines().nth(37), Some("X7"));
    }

    #[test]
    fn output_past_the_tail_budget_is_dropped_and_flagged() {
        let mut text = TerminalText::default();
        let line = "x".repeat(1023);
        for _ in 0..80 {
            text.append(&format!("{line}\n"));
        }
        assert!(text.truncated);
        assert!(text.text_content().len() <= TAIL_CHARS);
    }

    #[test]
    fn huge_cursor_moves_stay_bounded() {
        let mut text = TerminalText::default();
        text.append("a\nb\x1b[99999999Bc");
        assert_eq!(&*text.text_content(), "a\nbc", "moving down stops at the last line");
        text.append("\x1b[99999999Cd\x1b[99999999Ge");
        text.append(&format!("\x1b[{}B\x1b[{}C!", usize::MAX, usize::MAX));
        assert!(text.lines.iter().all(|line| line.len() <= TAIL_CHARS + 1));
        assert!(text.text_content().ends_with('!'));
    }
}
