use std::fmt;

use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize, // 1-indexed
    pub col: usize,  // 1-indexed, in grapheme clusters
}

impl Span {
    pub fn new(line: usize, col: usize) -> Self {
        Span { line, col }
    }
}

#[derive(Debug, Clone)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(node: T, span: Span) -> Self {
        Spanned { node, span }
    }
}

/// The QMCL Error Handler's diagnostic type: what went wrong, where, and
/// what to do about it.
#[derive(Debug, Clone)]
pub struct QmclError {
    pub message: String,
    pub span: Option<Span>,
    /// The general grammar rule that was broken.
    pub rule: Option<String>,
    /// Concrete, actionable fix(es) for this specific occurrence.
    pub suggestions: Vec<String>,
}

impl QmclError {
    pub fn new(message: impl Into<String>) -> Self {
        QmclError {
            message: message.into(),
            span: None,
            rule: None,
            suggestions: Vec::new(),
        }
    }

    pub fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn rule(mut self, rule: impl Into<String>) -> Self {
        self.rule = Some(rule.into());
        self
    }

    pub fn suggest(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestions.push(suggestion.into());
        self
    }

    /// Rust-compiler-style rendering: the message, a `-->` file:line:col,
    /// the offending source line with a caret under the exact spot, then
    /// suggested fix(es) under `help:`.
    pub fn render(&self, filename: &str, source: &str) -> String {
        let mut out = String::new();
        out.push_str("QMCL Error Handler\n");
        out.push_str(&format!("error: {}\n", self.message));

        if let Some(span) = self.span {
            let lines: Vec<&str> = source.lines().collect();
            // If the span points past the real content (e.g. an
            // end-of-file position that isn't an actual line in the
            // source), fall back to the last real line instead of
            // silently showing no snippet at all.
            let (line_no, col_no, line_text) = match lines.get(span.line.saturating_sub(1)) {
                Some(text) => (span.line, span.col, Some(*text)),
                None => match lines.last() {
                    Some(text) => (
                        lines.len(),
                        text.graphemes(true).count() + 1,
                        Some(*text),
                    ),
                    None => (span.line, span.col, None),
                },
            };

            out.push_str(&format!("  --> {}:{}:{}\n", filename, line_no, col_no));
            out.push_str(&format!("  file:   {}\n", filename));
            out.push_str(&format!("  line:   {}\n", line_no));
            out.push_str(&format!("  column: {}\n", col_no));
            if let Some(line_text) = line_text {
                let gutter = line_no.to_string();
                let pad = " ".repeat(gutter.len());
                out.push_str(&format!("{} |\n", pad));
                out.push_str(&format!("{} | {}\n", gutter, line_text));
                let caret_pad = " ".repeat(col_no.saturating_sub(1));
                out.push_str(&format!("{} | {}^\n", pad, caret_pad));
            }
        }

        if let Some(rule) = &self.rule {
            out.push_str(&format!("rule: {}\n", rule));
        }

        for (i, suggestion) in self.suggestions.iter().enumerate() {
            if i == 0 {
                out.push_str(&format!("suggestion(s): {}\n", suggestion));
            } else {
                out.push_str(&format!("               {}\n", suggestion));
            }
        }

        out
    }
}

impl fmt::Display for QmclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
